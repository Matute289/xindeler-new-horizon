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

use crate::{
    format::ServerIdentity,
    probe::{MAP_ASSET, Res},
};

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

#[cfg(test)]
pub fn log_is_ready(log: &str) -> bool { strip_ansi(log).contains(READY_LINE) }

fn tail(path: &Path, lines: usize) -> String {
    let text = strip_ansi(&fs::read_to_string(path).unwrap_or_default());
    let all: Vec<&str> = text.lines().collect();
    all[all.len().saturating_sub(lines)..].join("\n")
}

/// Incremental reader of the server log: only bytes appended since the last
/// poll are read (the log grows to megabytes while the world loads), and a
/// small stripped-text carry lets the ready line be found even when it
/// straddles two polls.
pub struct LogWatcher {
    path: PathBuf,
    offset: u64,
    carry: String,
}

impl LogWatcher {
    pub fn new(path: &Path) -> Self {
        Self {
            path: path.to_path_buf(),
            offset: 0,
            carry: String::new(),
        }
    }

    /// Read what was appended; `true` once the ready line has been seen.
    pub fn poll_ready(&mut self) -> bool {
        use std::io::{Read, Seek, SeekFrom};
        let Ok(mut f) = fs::File::open(&self.path) else {
            return false;
        };
        let len = f.metadata().map_or(0, |m| m.len());
        if len < self.offset {
            // Truncated (a relaunch): start over.
            self.offset = 0;
            self.carry.clear();
        }
        if f.seek(SeekFrom::Start(self.offset)).is_err() {
            return false;
        }
        let mut buf = Vec::new();
        if f.read_to_end(&mut buf).is_err() {
            return false;
        }
        self.offset += buf.len() as u64;
        let mut text = std::mem::take(&mut self.carry);
        text.push_str(&strip_ansi(&String::from_utf8_lossy(&buf)));
        let ready = text.contains(READY_LINE);
        // Keep the last READY_LINE.len() - 1 bytes (on a char boundary).
        let mut cut = text.len().saturating_sub(READY_LINE.len());
        while !text.is_char_boundary(cut) {
            cut += 1;
        }
        self.carry = text[cut..].to_string();
        ready
    }
}

/// Deadline for the one-shot `admin add` run.
const ADMIN_ADD_TIMEOUT: Duration = Duration::from_secs(120);

/// Whether an early-exit log tail looks like a lost race for a port.
pub fn is_bind_error(log_tail: &str) -> bool {
    let l = log_tail.to_lowercase();
    l.contains("address already in use")
        || l.contains("addrinuse")
        || l.contains("os error 48")
        || l.contains("os error 98")
        || l.contains("only one usage of each socket")
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
        let log = dir.join("server.log");
        let inner = Arc::new(Inner {
            child: Mutex::new(None),
            dir: dir.clone(),
            log: log.clone(),
            keep_log_to: o.keep_log_to.clone(),
        });
        // Signal cleanup goes in BEFORE anything is spawned, and every child
        // is registered in `inner` while its spawn lock is still held, so an
        // interrupt at any moment (including the up-to-10 minute ready wait)
        // finds and kills it.
        install_signal_cleanup(Arc::clone(&inner));
        // From here on Drop cleans up the directory on every exit path.
        let mut me = Self {
            inner: Arc::clone(&inner),
            port: 0,
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
        let write_settings = |port: u16, web_port: u16| -> Res<()> {
            fs::write(
                ud.join("server/server_config/settings.ron"),
                game_settings_ron(port, o.seed, o.view_distance),
            )?;
            fs::write(
                ud.join("server-cli/settings.ron"),
                cli_settings_ron(web_port),
            )?;
            Ok(())
        };
        let (port, web_port) = free_ports()?;
        write_settings(port, web_port)?;

        // Register the bot as admin (no auth, so the name is the identity).
        // Spawned, registered and polled against a deadline: a wedged binary
        // cannot hang the run.
        let admin_log = dir.join("admin-add.log");
        let logf = fs::File::create(&admin_log)?;
        {
            let mut g = inner.child.lock().map_err(|e| e.to_string())?;
            *g = Some(
                cmd(&[
                    "--no-auth",
                    "--non-interactive",
                    "admin",
                    "add",
                    BOT_USER,
                    "admin",
                ])
                .stdout(Stdio::from(logf.try_clone()?))
                .stderr(Stdio::from(logf))
                .spawn()?,
            );
        }
        let t_admin = Instant::now();
        let status = loop {
            match me.exited()? {
                Some(s) => break s,
                None if t_admin.elapsed() > ADMIN_ADD_TIMEOUT => {
                    return Err(format!(
                        "`admin add` did not finish in {}s: {}",
                        ADMIN_ADD_TIMEOUT.as_secs(),
                        tail(&admin_log, 15)
                    )
                    .into());
                },
                None => std::thread::sleep(Duration::from_millis(100)),
            }
        };
        // Forget the finished child (nothing to kill).
        let _ = inner.child.lock().map(|mut g| g.take());
        if !status.success() {
            return Err(format!("`admin add` failed ({status}): {}", tail(&admin_log, 15)).into());
        }

        let mut port = port;
        for attempt in 0..2 {
            let logf = fs::File::create(&log)?;
            {
                let mut g = inner.child.lock().map_err(|e| e.to_string())?;
                let child = cmd(&["--no-auth", "--non-interactive"])
                    .stdout(Stdio::from(logf.try_clone()?))
                    .stderr(Stdio::from(logf))
                    .spawn()?;
                eprintln!(
                    "scratch server pid {} on 127.0.0.1:{port} (userdata {})",
                    child.id(),
                    ud.display()
                );
                *g = Some(child);
            }
            me.port = port;
            let t0 = Instant::now();
            let mut watcher = LogWatcher::new(&log);
            loop {
                if watcher.poll_ready() {
                    eprintln!("server ready in {:.0}s", t0.elapsed().as_secs_f32());
                    return Ok(me);
                }
                if let Some(status) = me.exited()? {
                    let t = tail(&log, 25);
                    if attempt == 0 && is_bind_error(&t) {
                        eprintln!("port race (server exited: {status}); retrying with new ports");
                        let _ = inner.child.lock().map(|mut g| g.take());
                        let web_port;
                        (port, web_port) = free_ports()?;
                        write_settings(port, web_port)?;
                        break;
                    }
                    return Err(format!("server exited early ({status}); log tail:\n{t}").into());
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
        Err("server failed to start after a port retry".into())
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

/// What the server binary reports about itself (`--version`) plus its content
/// hash and mtime, recorded in the dump header.
pub fn server_identity(bin: &Path, probe_commit: &str) -> ServerIdentity {
    use sha2::{Digest, Sha256};
    let version = Command::new(bin)
        .arg("--version")
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .ok()
        .and_then(|o| {
            String::from_utf8_lossy(&o.stdout)
                .lines()
                .next()
                .map(str::to_owned)
        });
    let sha256 = fs::File::open(bin)
        .ok()
        .map(|mut f| {
            let mut h = Sha256::new();
            let mut buf = vec![0u8; 1 << 20];
            while let Ok(n) = std::io::Read::read(&mut f, &mut buf) {
                if n == 0 {
                    break;
                }
                h.update(&buf[..n]);
            }
            h.finalize().iter().map(|b| format!("{b:02x}")).collect()
        })
        .unwrap_or_default();
    let mtime_unix = fs::metadata(bin)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_secs());
    ServerIdentity {
        version,
        sha256,
        mtime_unix,
        probe_commit: probe_commit.to_string(),
    }
}

/// The 7-40 hex chars of the hash in a `--version` line.
fn version_hash(v: &str) -> Option<&str> {
    v.split_whitespace()
        .find(|w| (7..=40).contains(&w.len()) && w.bytes().all(|b| b.is_ascii_hexdigit()))
}

/// A warning when the server binary was demonstrably built from another
/// commit than the probe (hash prefix comparison; the engine's own version
/// string only carries the commit, not a dirty marker, and is baked at its
/// build, so this is a hint, not a proof).
pub fn version_skew_warning(id: &ServerIdentity) -> Option<String> {
    let Some(v) = &id.version else {
        return Some(
            "could not read the server binary's --version; cannot compare its commit".into(),
        );
    };
    let h = version_hash(v)?;
    let probe = id.probe_commit.trim_end_matches("+dirty");
    (!probe.starts_with(h) && !h.starts_with(probe)).then(|| {
        format!(
            "server binary reports `{v}` but the probe is {}: rebuild both from the same commit \
             (`cargo build -p xindeler-server-cli -p xindeler-terrain-probe`) or a fast-vs-client \
             difference may be version skew. Asset hashes agree by construction (the scratch \
             server runs on the probe's --assets).",
            id.probe_commit
        )
    })
}

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

    #[test]
    fn log_watcher_reads_only_new_bytes_and_spans_polls() {
        use std::io::Write;
        let dir = std::env::temp_dir().join(format!("tprobe-logw-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("l.log");
        fs::write(&path, "starting\n").unwrap();
        let mut w = LogWatcher::new(&path);
        assert!(!w.poll_ready());
        let mut f = fs::OpenOptions::new().append(true).open(&path).unwrap();
        // The ready line arrives in two pieces, with colour codes.
        f.write_all(b"\x1b[2mINFO\x1b[0m Server is ready to ac")
            .unwrap();
        assert!(!w.poll_ready());
        assert_eq!(w.offset, fs::metadata(&path).unwrap().len());
        f.write_all(b"cept connections.\n").unwrap();
        assert!(w.poll_ready());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn version_skew_is_detected_from_the_version_line() {
        let id = |v: Option<&str>, c: &str| ServerIdentity {
            version: v.map(str::to_owned),
            sha256: String::new(),
            mtime_unix: None,
            probe_commit: c.to_owned(),
        };
        let full = "1d1069c881a1b6d9b9e0e0e0e0e0e0e0e0e0e0e0";
        let line = Some("Veloren server CLI 1d1069c8 [2026-10-03]");
        assert!(version_skew_warning(&id(line, full)).is_none());
        assert!(version_skew_warning(&id(line, &format!("{full}+dirty"))).is_none());
        let other = "8001a621812f05f93a893c18539f84d5d16331a0";
        let w = version_skew_warning(&id(line, other)).unwrap();
        assert!(w.contains("1d1069c8") && w.contains("8001a621"), "{w}");
        assert!(version_skew_warning(&id(None, other)).is_some());
        assert_eq!(
            version_hash("Veloren server CLI 1d1069c8 [2026-10-03]"),
            Some("1d1069c8")
        );
    }

    #[test]
    fn bind_errors_are_recognised() {
        assert!(is_bind_error("Error: Address already in use (os error 48)"));
        assert!(is_bind_error("bind: AddrInUse"));
        assert!(!is_bind_error("panic: asset not found"));
    }

    /// Helper process for
    /// [`sigterm_during_startup_kills_the_child_and_removes_the_dir`]:
    /// starts a scratch server whose "binary" never becomes ready. A no-op
    /// unless the parent test set `TPROBE_SIGTEST_BIN`.
    #[test]
    fn sigterm_helper() {
        let Some(bin) = std::env::var_os("TPROBE_SIGTEST_BIN") else {
            return;
        };
        let r = ScratchServer::start(&ServerOpts {
            server_bin: PathBuf::from(bin),
            assets: std::env::temp_dir(),
            seed: 0,
            view_distance: 8,
            ready_timeout: Duration::from_secs(600),
            keep_log_to: None,
        });
        panic!(
            "start returned {:?} instead of being interrupted",
            r.map(|_| ())
        );
    }

    /// A SIGTERM during the ready wait must kill our server child and delete
    /// the scratch dir (the cleanup used to be installed only after
    /// `start` returned, leaving an orphan for up to the whole timeout).
    #[cfg(unix)]
    #[test]
    fn sigterm_during_startup_kills_the_child_and_removes_the_dir() {
        use std::{
            io::{BufRead, BufReader},
            os::unix::fs::PermissionsExt,
        };
        let work = std::env::temp_dir().join(format!("tprobe-sigtest-{}", std::process::id()));
        fs::create_dir_all(&work).unwrap();
        let bin = work.join("fake-server.sh");
        // `admin add` exits at once; the server itself sleeps and never
        // prints the ready line.
        fs::write(
            &bin,
            "#!/bin/sh\nfor a in \"$@\"; do [ \"$a\" = admin ] && exit 0; done\nexec sleep 300\n",
        )
        .unwrap();
        fs::set_permissions(&bin, fs::Permissions::from_mode(0o755)).unwrap();
        let mut helper = Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "scratch_server::tests::sigterm_helper",
                "--nocapture",
                "--test-threads=1",
            ])
            .env("TPROBE_SIGTEST_BIN", &bin)
            .stderr(Stdio::piped())
            .stdout(Stdio::null())
            .spawn()
            .unwrap();
        let mut lines = BufReader::new(helper.stderr.take().unwrap()).lines();
        let (mut pid, mut dir) = (None, None);
        for l in lines.by_ref().map_while(Result::ok) {
            if let Some(rest) = l.strip_prefix("scratch server pid ") {
                pid = rest.split(' ').next().map(str::to_owned);
                dir = rest
                    .split("(userdata ")
                    .nth(1)
                    .and_then(|u| u.strip_suffix(')'))
                    .and_then(|u| Path::new(u).parent().map(Path::to_path_buf));
                break;
            }
        }
        let (pid, dir) = (pid.expect("server pid line"), dir.expect("userdata dir"));
        let alive = |pid: &str| {
            Command::new("kill")
                .args(["-0", pid])
                .stderr(Stdio::null())
                .status()
                .is_ok_and(|s| s.success())
        };
        assert!(
            alive(&pid) && dir.exists(),
            "server must be running before the signal"
        );
        assert!(
            Command::new("kill")
                .args(["-TERM", &helper.id().to_string()])
                .status()
                .unwrap()
                .success()
        );
        let t0 = Instant::now();
        while helper.try_wait().unwrap().is_none() {
            assert!(
                t0.elapsed() < Duration::from_secs(30),
                "helper ignored SIGTERM"
            );
            std::thread::sleep(Duration::from_millis(50));
        }
        assert!(!alive(&pid), "server child {pid} survived SIGTERM");
        assert!(
            !dir.exists(),
            "scratch dir {} survived SIGTERM",
            dir.display()
        );
        let _ = fs::remove_dir_all(&work);
    }
}
