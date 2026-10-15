//! Test-support: machine-readable verdicts of the real-asset ground tests.
//!
//! When the environment variable `XINDELER_BATTERY_REPORT=<path>` is set, a
//! test that holds a [`BatteryRow`] writes one JSON record per battery row
//! into `<path>` (a JSON array, one record per line). Without the variable
//! nothing is read, written or stored: the tests behave exactly as before.
//!
//! Record schema (version 2; readers should ignore unknown keys):
//!
//! ```text
//! {"schema":2,
//!  "row":"B-T8",                       // battery row id
//!  "fixture":"wilderness_river_3x3",   // arena or programmatic fixture
//!  "test":"authored_raster::ground_seam_real_world_tests::exact_patch_...",
//!  "metrics":{"seam_pairs":12,"walls":0,"refused_when_low":true},
//!  "thresholds":{"walls_max":0},       // only what the test asserts
//!  "pass":true,                        // false if the test panicked
//!  "engine_commit":"<sha>",            // XINDELER_BATTERY_ENGINE_COMMIT or null
//!  "timestamp":1760000000}             // seconds since the Unix epoch
//! ```
//!
//! `pass` means "the test did not panic", not "every threshold was met"; it
//! is computed when the [`BatteryRow`] is dropped. A test that panics reports
//! `false` with the metrics gathered up to that point (partial); assertions
//! themselves are untouched. A record is keyed by `(row, fixture, test)`;
//! re-running replaces it, so separate `cargo test` invocations can write to
//! the same file and the result is a merged set. Records of removed or
//! renamed tests persist (they keep their old key), so a runner doing a full
//! run should delete the report file first.
//!
//! Concurrency: parallel tests of one process are serialised in-process, and
//! other processes are guarded by an advisory `<path>.lock` file plus an
//! atomic write (`<path>.tmp.<pid>` then rename), so a reader never sees a
//! half-written file. Still, run one process per report file at a time
//! (serially); a corrupt line found on read is reported on stderr and dropped.

use std::{
    collections::BTreeMap,
    fmt::Write as _,
    path::{Path, PathBuf},
    sync::Mutex,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

/// Serialises the read-modify-write of the report file between the parallel
/// tests of one process.
static FILE_LOCK: Mutex<()> = Mutex::new(());

fn report_path() -> Option<PathBuf> {
    std::env::var_os("XINDELER_BATTERY_REPORT")
        .filter(|p| !p.is_empty())
        .map(PathBuf::from)
}

fn esc(s: &str) -> String {
    let mut o = String::with_capacity(s.len() + 2);
    o.push('"');
    for c in s.chars() {
        match c {
            '"' => o.push_str("\\\""),
            '\\' => o.push_str("\\\\"),
            c if (c as u32) < 0x20 => {
                let _ = write!(o, "\\u{:04x}", c as u32);
            },
            c => o.push(c),
        }
    }
    o.push('"');
    o
}

fn num(v: f64) -> String {
    if v.is_finite() {
        format!("{v}")
    } else {
        "null".to_string()
    }
}

/// The value of `"key":"..."` in a record line written by [`render`].
fn string_field(line: &str, key: &str) -> Option<String> {
    let pat = format!("\"{key}\":\"");
    let start = line.find(&pat)? + pat.len();
    let mut out = String::new();
    let mut chars = line[start..].chars();
    while let Some(c) = chars.next() {
        match c {
            '"' => return Some(out),
            '\\' => match chars.next()? {
                'n' => out.push('\n'),
                'r' => out.push('\r'),
                't' => out.push('\t'),
                'u' => {
                    let hex: String = chars.by_ref().take(4).collect();
                    out.push(char::from_u32(u32::from_str_radix(&hex, 16).ok()?)?);
                },
                c => out.push(c),
            },
            c => out.push(c),
        }
    }
    None
}

fn record_key(line: &str) -> Option<(String, String, String)> {
    Some((
        string_field(line, "row")?,
        string_field(line, "fixture")?,
        string_field(line, "test")?,
    ))
}

/// One battery row's verdict, written when dropped (see the module docs).
pub struct BatteryRow {
    enabled: bool,
    row: &'static str,
    fixture: &'static str,
    test: String,
    metrics: Vec<(&'static str, String)>,
    thresholds: Vec<(&'static str, String)>,
}

impl BatteryRow {
    /// `test` is the module-relative path of the test fn, see
    /// [`battery_row!`].
    pub fn new(row: &'static str, fixture: &'static str, test: String) -> Self {
        Self {
            enabled: report_path().is_some(),
            row,
            fixture,
            test,
            metrics: Vec::new(),
            thresholds: Vec::new(),
        }
    }

    /// A measured number.
    pub fn metric(&mut self, name: &'static str, value: impl Into<f64>) -> &mut Self {
        if self.enabled {
            self.metrics.push((name, num(value.into())));
        }
        self
    }

    /// A measured yes/no fact (a refusal happened, a query answered so).
    pub fn fact(&mut self, name: &'static str, value: bool) -> &mut Self {
        if self.enabled {
            self.metrics.push((name, value.to_string()));
        }
        self
    }

    /// A numeric threshold the test asserts on.
    pub fn threshold(&mut self, name: &'static str, value: impl Into<f64>) -> &mut Self {
        if self.enabled {
            self.thresholds.push((name, num(value.into())));
        }
        self
    }

    /// Whether a report is being collected. Callers gate expensive metric
    /// computation on this so nothing is evaluated without the env var.
    pub fn enabled(&self) -> bool { self.enabled }

    fn render(&self, pass: bool) -> String {
        let obj = |kv: &[(&str, String)]| {
            let body: Vec<String> = kv.iter().map(|(k, v)| format!("{}:{v}", esc(k))).collect();
            format!("{{{}}}", body.join(","))
        };
        format!(
            "{{\"schema\":2,\"row\":{},\"fixture\":{},\"test\":{},\"metrics\":{},\"thresholds\":\
             {},\"pass\":{pass},\"engine_commit\":{},\"timestamp\":{}}}",
            esc(self.row),
            esc(self.fixture),
            esc(&self.test),
            obj(&self.metrics),
            obj(&self.thresholds),
            std::env::var("XINDELER_BATTERY_ENGINE_COMMIT")
                .ok()
                .filter(|c| !c.is_empty())
                .map_or_else(|| "null".to_string(), |c| esc(&c)),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map_or(0, |d| d.as_secs()),
        )
    }

    fn write(&self, path: &Path, pass: bool) -> std::io::Result<()> {
        merge_record(path, &self.render(pass))
    }
}

/// Advisory cross-process lock: `<path>.lock` created with `create_new`,
/// retried for a bounded time, then taken over (a crashed run must not wedge
/// the report forever).
struct FileLock(PathBuf);

impl FileLock {
    fn acquire(path: &Path) -> Self {
        let mut name = path.as_os_str().to_owned();
        name.push(".lock");
        let lock = PathBuf::from(name);
        for _ in 0..200 {
            if std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&lock)
                .is_ok()
            {
                return Self(lock);
            }
            std::thread::sleep(Duration::from_millis(25));
        }
        eprintln!(
            "battery report: stale lock {} taken over after 5 s",
            lock.display()
        );
        let _ = std::fs::write(&lock, b"");
        Self(lock)
    }
}

impl Drop for FileLock {
    fn drop(&mut self) { let _ = std::fs::remove_file(&self.0); }
}

/// Read the report at `path`, insert/replace `record`, and write it back
/// atomically.
fn merge_record(path: &Path, record: &str) -> std::io::Result<()> {
    let _guard = FILE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(dir) = path.parent().filter(|d| !d.as_os_str().is_empty()) {
        std::fs::create_dir_all(dir)?;
    }
    let _file_lock = FileLock::acquire(path);
    let mut records: BTreeMap<(String, String, String), String> = BTreeMap::new();
    if let Ok(old) = std::fs::read_to_string(path) {
        for line in old.lines() {
            let line = line.trim().trim_end_matches(',');
            if line.is_empty() || line == "[" || line == "]" {
                continue;
            }
            match record_key(line).filter(|_| line.starts_with('{') && line.ends_with('}')) {
                Some(k) => {
                    records.insert(k, line.to_string());
                },
                None => eprintln!(
                    "battery report: dropping a corrupt line in {}: {}",
                    path.display(),
                    line.chars().take(80).collect::<String>()
                ),
            }
        }
    }
    if let Some(k) = record_key(record) {
        records.insert(k, record.to_string());
    }
    let mut out = String::from("[\n");
    let n = records.len();
    for (i, line) in records.values().enumerate() {
        out.push_str(line);
        out.push_str(if i + 1 < n { ",\n" } else { "\n" });
    }
    out.push_str("]\n");
    let mut tmp_name = path.as_os_str().to_owned();
    tmp_name.push(format!(".tmp.{}", std::process::id()));
    let tmp = PathBuf::from(tmp_name);
    std::fs::write(&tmp, out)?;
    std::fs::rename(&tmp, path).inspect_err(|_| {
        let _ = std::fs::remove_file(&tmp);
    })
}

impl Drop for BatteryRow {
    fn drop(&mut self) {
        if !self.enabled {
            return;
        }
        let Some(path) = report_path() else { return };
        let pass = !std::thread::panicking();
        if let Err(e) = self.write(&path, pass) {
            eprintln!("battery report: cannot write {}: {e}", path.display());
        }
    }
}

/// `battery_row!("B-T8", "fixture_name", "test_fn_name")`: a [`BatteryRow`]
/// whose test id is `<module path without the crate>::<fn>`.
macro_rules! battery_row {
    ($row:expr, $fixture:expr, $fn_name:expr) => {{
        let module = module_path!();
        let module = module.split_once("::").map_or(module, |(_, rest)| rest);
        $crate::authored_raster::battery_report::BatteryRow::new(
            $row,
            $fixture,
            format!("{module}::{}", $fn_name),
        )
    }};
}
pub(crate) use battery_row;

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("battery_report_{}_{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        d.join("report.json")
    }

    fn row(test: &str) -> BatteryRow {
        BatteryRow {
            enabled: true,
            row: "B-T0",
            fixture: "fx",
            test: test.to_string(),
            metrics: Vec::new(),
            thresholds: Vec::new(),
        }
    }

    #[test]
    fn escapes_and_unicode_round_trip() {
        let s = "a\"b\\c\u{1}\n\té";
        let line = format!("{{\"test\":{}}}", esc(s));
        assert_eq!(string_field(&line, "test").as_deref(), Some(s));
        assert!(!line.contains('\n'));
    }

    #[test]
    fn non_finite_numbers_are_null_and_record_is_valid_shape() {
        let mut r = row("t");
        r.metric("nan", f64::NAN)
            .metric("inf", f64::INFINITY)
            .metric("x", 1.5)
            .fact("f", true);
        let line = r.render(true);
        assert!(line.contains("\"nan\":null,\"inf\":null,\"x\":1.5,\"f\":true"));
        assert!(line.contains("\"schema\":2") && line.contains("\"timestamp\":"));
        assert!(line.contains("\"engine_commit\":"));
    }

    #[test]
    fn merge_replaces_same_key_and_keeps_others() {
        let p = tmp("merge");
        let mut a = row("a");
        a.metric("v", 1);
        merge_record(&p, &a.render(true)).unwrap();
        merge_record(&p, &row("b").render(true)).unwrap();
        let mut a2 = row("a");
        a2.metric("v", 2);
        merge_record(&p, &a2.render(false)).unwrap();
        let txt = std::fs::read_to_string(&p).unwrap();
        assert_eq!(txt.lines().filter(|l| l.starts_with('{')).count(), 2);
        assert!(txt.contains("\"v\":2") && !txt.contains("\"v\":1"));
        assert!(txt.starts_with("[\n") && txt.ends_with("]\n"));
        let mut lock = p.as_os_str().to_owned();
        lock.push(".lock");
        assert!(!PathBuf::from(lock).exists());
        let _ = std::fs::remove_dir_all(p.parent().unwrap());
    }

    #[test]
    fn truncated_file_is_tolerated() {
        let p = tmp("trunc");
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        let good = row("good").render(true);
        std::fs::write(
            &p,
            format!("[\n{good},\n{{\"schema\":2,\"row\":\"B-T0\",\"fixt"),
        )
        .unwrap();
        merge_record(&p, &row("new").render(true)).unwrap();
        let txt = std::fs::read_to_string(&p).unwrap();
        assert_eq!(txt.lines().filter(|l| l.starts_with('{')).count(), 2);
        assert!(!txt.contains("fixt\n"));
        let _ = std::fs::remove_dir_all(p.parent().unwrap());
    }
}
