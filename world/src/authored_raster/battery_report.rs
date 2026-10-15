//! Test-support: machine-readable verdicts of the real-asset ground tests.
//!
//! When the environment variable `XINDELER_BATTERY_REPORT=<path>` is set, a
//! test that holds a [`BatteryRow`] writes one JSON record per battery row
//! into `<path>` (a JSON array, one record per line). Without the variable
//! nothing is read, written or stored: the tests behave exactly as before.
//!
//! Record schema (version 1):
//!
//! ```text
//! {"schema":1,
//!  "row":"B-T8",                       // battery row id
//!  "fixture":"wilderness_river_3x3",   // arena or programmatic fixture
//!  "test":"authored_raster::ground_seam_real_world_tests::exact_patch_...",
//!  "metrics":{"seam_pairs":12,"walls":0,"refused_when_low":true},
//!  "thresholds":{"walls_max":0},       // only what the test asserts
//!  "pass":true}                        // false if the test panicked
//! ```
//!
//! `pass` is computed when the [`BatteryRow`] is dropped: a test that panics
//! (an assertion failed) reports `false` with the metrics gathered up to that
//! point; assertions themselves are untouched. A record is keyed by
//! `(row, fixture, test)`; re-running replaces it, so separate `cargo test`
//! invocations can write to the same file and the result is a merged set.

use std::{collections::BTreeMap, fmt::Write as _, path::PathBuf, sync::Mutex};

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
            '\\' => out.push(chars.next()?),
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

    fn render(&self, pass: bool) -> String {
        let obj = |kv: &[(&str, String)]| {
            let body: Vec<String> = kv.iter().map(|(k, v)| format!("{}:{v}", esc(k))).collect();
            format!("{{{}}}", body.join(","))
        };
        format!(
            "{{\"schema\":1,\"row\":{},\"fixture\":{},\"test\":{},\"metrics\":{},\"thresholds\":\
             {},\"pass\":{pass}}}",
            esc(self.row),
            esc(self.fixture),
            esc(&self.test),
            obj(&self.metrics),
            obj(&self.thresholds),
        )
    }

    fn write(&self, path: &std::path::Path, pass: bool) -> std::io::Result<()> {
        let _guard = FILE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let mut records: BTreeMap<(String, String, String), String> = BTreeMap::new();
        if let Ok(old) = std::fs::read_to_string(path) {
            for line in old.lines() {
                let line = line.trim().trim_end_matches(',');
                if line.starts_with('{')
                    && let Some(k) = record_key(line)
                {
                    records.insert(k, line.to_string());
                }
            }
        }
        let mine = self.render(pass);
        if let Some(k) = record_key(&mine) {
            records.insert(k, mine);
        }
        let mut out = String::from("[\n");
        let n = records.len();
        for (i, line) in records.values().enumerate() {
            out.push_str(line);
            out.push_str(if i + 1 < n { ",\n" } else { "\n" });
        }
        out.push_str("]\n");
        if let Some(dir) = path.parent().filter(|d| !d.as_os_str().is_empty()) {
            std::fs::create_dir_all(dir)?;
        }
        std::fs::write(path, out)
    }
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
