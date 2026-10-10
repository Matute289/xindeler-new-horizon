//! RON schema for shot scripts, plus the validator that runs after parsing.
//!
//! Parsing is strict: unknown fields and unknown step kinds are refused by
//! serde, and every value that serde accepts is then range-checked by
//! [`ShotScript::validate`]. Errors always name the script file and, when it
//! can be determined, the line of the offending step.

use serde::Deserialize;
use std::{collections::HashSet, fmt, path::Path};

/// The only script format version this build understands.
pub const SCRIPT_VERSION: u32 = 1;

/// Maximum length of a screenshot name or a sample label, in bytes.
pub const MAX_NAME_LEN: usize = 64;

/// Times of day accepted by [`Step::Time`]; the same named times the `/time`
/// admin command understands. Numeric times are deliberately not accepted, so
/// the value can never smuggle extra command arguments.
pub const TIME_NAMES: &[&str] = &[
    "midnight", "night", "dawn", "morning", "day", "noon", "dusk",
];

/// Accepted window size range (both dimensions), in pixels.
const WINDOW_MIN: u32 = 320;
const WINDOW_MAX: u32 = 7680;

/// A complete shot script.
#[derive(Clone, Debug, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct ShotScript {
    pub version: u32,
    /// Window size in pixels, fixed so screenshots are comparable run to run.
    pub window: (u32, u32),
    pub defaults: Defaults,
    pub steps: Vec<Step>,
}

/// Per-script defaults, overridable per step where the step allows it.
#[derive(Clone, Debug, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Defaults {
    /// Terrain and entity view distance, in chunks.
    pub view_distance: u32,
    /// Radius, in chunks, that `WaitForTerrain` waits for.
    pub terrain_radius_chunks: u32,
    /// How long `WaitForTerrain` waits before giving up, in seconds.
    pub terrain_timeout_s: f32,
    /// Frames rendered after terrain is ready and before a screenshot.
    pub settle_frames: u32,
    pub hide_hud: bool,
}

/// One step of a script. There is intentionally no raw-command step: a
/// script can only do what these whitelisted steps describe.
#[derive(Clone, Debug, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub enum Step {
    /// Set the time of day (one of [`TIME_NAMES`]).
    Time(String),
    /// Teleport the player to a world position.
    Teleport { wpos: (f32, f32, f32) },
    /// Teleport to a Cromatolis map pixel, `z_above_ground` metres above the
    /// ground there.
    CromatolisTeleport {
        px: u32,
        py: u32,
        z_above_ground: f32,
    },
    /// Wait until terrain around the player is loaded and meshed. Omitted
    /// fields use [`Defaults`].
    WaitForTerrain {
        #[serde(default)]
        radius_chunks: Option<u32>,
        #[serde(default)]
        timeout_s: Option<f32>,
    },
    /// Orbit camera relative to the player.
    Camera {
        yaw_deg: f32,
        pitch_deg: f32,
        distance: f32,
    },
    /// Free-flying camera at `cam`, looking at `look_at` (world positions).
    Viewpoint {
        cam: (f32, f32, f32),
        look_at: (f32, f32, f32),
    },
    /// Show or hide the minimap.
    Minimap { show: bool },
    /// Wait a fixed number of seconds.
    WaitSeconds(f32),
    /// Marker for memory / frame-time aggregation.
    Sample { label: String },
    /// Take a screenshot; `name` becomes the file stem.
    Screenshot { name: String },
}

impl Step {
    /// The step's kind, as written in the script.
    pub fn kind(&self) -> &'static str {
        match self {
            Step::Time(_) => "Time",
            Step::Teleport { .. } => "Teleport",
            Step::CromatolisTeleport { .. } => "CromatolisTeleport",
            Step::WaitForTerrain { .. } => "WaitForTerrain",
            Step::Camera { .. } => "Camera",
            Step::Viewpoint { .. } => "Viewpoint",
            Step::Minimap { .. } => "Minimap",
            Step::WaitSeconds(_) => "WaitSeconds",
            Step::Sample { .. } => "Sample",
            Step::Screenshot { .. } => "Screenshot",
        }
    }
}

/// A parse or validation error, naming the script file and, when known, the
/// line (and column) it refers to.
#[derive(Clone, Debug, PartialEq)]
pub struct ScriptError {
    pub file: String,
    pub line: Option<usize>,
    pub col: Option<usize>,
    pub message: String,
}

impl fmt::Display for ScriptError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.file)?;
        if let Some(line) = self.line {
            write!(f, ":{line}")?;
            if let Some(col) = self.col {
                write!(f, ":{col}")?;
            }
        }
        write!(f, ": {}", self.message)
    }
}

impl std::error::Error for ScriptError {}

impl ShotScript {
    /// Read, parse and validate a script file.
    pub fn load(path: &Path) -> Result<Self, ScriptError> {
        let file = path.display().to_string();
        let src = std::fs::read_to_string(path).map_err(|e| ScriptError {
            file: file.clone(),
            line: None,
            col: None,
            message: format!("cannot read script: {e}"),
        })?;
        Self::parse(&src, &file)
    }

    /// Parse and validate script source. `file` is only used in errors.
    pub fn parse(src: &str, file: &str) -> Result<Self, ScriptError> {
        // `implicit_some` lets optional fields be written as plain values
        // (`radius_chunks: 8`) instead of `Some(8)`.
        let options = ron::Options::default()
            .with_default_extension(ron::extensions::Extensions::IMPLICIT_SOME);
        let script: ShotScript = options.from_str(src).map_err(|e| ScriptError {
            file: file.to_string(),
            line: Some(e.span.start.line),
            col: Some(e.span.start.col),
            message: e.code.to_string(),
        })?;
        script.validate(src, file)?;
        Ok(script)
    }

    /// Range-check every value. `src` is used only to find step lines for
    /// error messages.
    pub fn validate(&self, src: &str, file: &str) -> Result<(), ScriptError> {
        let top = |message: String| ScriptError {
            file: file.to_string(),
            line: None,
            col: None,
            message,
        };

        if self.version != SCRIPT_VERSION {
            return Err(top(format!(
                "unsupported version {} (this build understands version {SCRIPT_VERSION})",
                self.version
            )));
        }
        let (w, h) = self.window;
        if !(WINDOW_MIN..=WINDOW_MAX).contains(&w) || !(WINDOW_MIN..=WINDOW_MAX).contains(&h) {
            return Err(top(format!(
                "window ({w}, {h}) out of range: both sides must be {WINDOW_MIN}..={WINDOW_MAX}"
            )));
        }
        let d = &self.defaults;
        if d.view_distance == 0 {
            return Err(top("defaults.view_distance must be at least 1".into()));
        }
        if d.terrain_radius_chunks == 0 {
            return Err(top(
                "defaults.terrain_radius_chunks must be at least 1".into()
            ));
        }
        positive("defaults.terrain_timeout_s", d.terrain_timeout_s).map_err(top)?;
        if self.steps.is_empty() {
            return Err(top("script has no steps".into()));
        }

        let lines = StepLines::new(src);
        let mut seen_kind: Vec<(&'static str, usize)> = Vec::new();
        let mut screenshot_names: HashSet<&str> = HashSet::new();
        for (index, step) in self.steps.iter().enumerate() {
            let kind = step.kind();
            // Which occurrence of this kind this step is, to locate its line.
            let nth = match seen_kind.iter_mut().find(|(k, _)| *k == kind) {
                Some((_, n)) => {
                    *n += 1;
                    *n
                },
                None => {
                    seen_kind.push((kind, 0));
                    0
                },
            };
            let at = |message: String| ScriptError {
                file: file.to_string(),
                line: lines.find(kind, nth),
                col: None,
                message: format!("step {} ({kind}): {message}", index + 1),
            };
            validate_step(step).map_err(at)?;
            if let Step::Screenshot { name } = step
                && !screenshot_names.insert(name.as_str())
            {
                return Err(at(format!("duplicate screenshot name {name:?}")));
            }
        }
        Ok(())
    }
}

fn finite(what: &str, v: f32) -> Result<(), String> {
    if v.is_finite() {
        Ok(())
    } else {
        Err(format!("{what} must be a finite number, got {v}"))
    }
}

fn positive(what: &str, v: f32) -> Result<(), String> {
    finite(what, v)?;
    if v > 0.0 {
        Ok(())
    } else {
        Err(format!("{what} must be greater than 0, got {v}"))
    }
}

fn finite3(what: &str, v: (f32, f32, f32)) -> Result<(), String> {
    finite(&format!("{what}.x"), v.0)?;
    finite(&format!("{what}.y"), v.1)?;
    finite(&format!("{what}.z"), v.2)
}

fn validate_step(step: &Step) -> Result<(), String> {
    match step {
        Step::Time(name) => {
            if !TIME_NAMES.contains(&name.as_str()) {
                return Err(format!(
                    "unknown time {name:?}; expected one of {}",
                    TIME_NAMES.join(", ")
                ));
            }
        },
        Step::Teleport { wpos } => finite3("wpos", *wpos)?,
        Step::CromatolisTeleport { z_above_ground, .. } => {
            finite("z_above_ground", *z_above_ground)?
        },
        Step::WaitForTerrain {
            radius_chunks,
            timeout_s,
        } => {
            if *radius_chunks == Some(0) {
                return Err("radius_chunks must be at least 1".into());
            }
            if let Some(t) = timeout_s {
                positive("timeout_s", *t)?;
            }
        },
        Step::Camera {
            yaw_deg,
            pitch_deg,
            distance,
        } => {
            finite("yaw_deg", *yaw_deg)?;
            finite("pitch_deg", *pitch_deg)?;
            if !(-90.0..=90.0).contains(pitch_deg) {
                return Err(format!("pitch_deg must be in -90..=90, got {pitch_deg}"));
            }
            finite("distance", *distance)?;
            if *distance < 0.0 {
                return Err(format!("distance must not be negative, got {distance}"));
            }
        },
        Step::Viewpoint { cam, look_at } => {
            finite3("cam", *cam)?;
            finite3("look_at", *look_at)?;
            if cam == look_at {
                return Err("cam and look_at must differ".into());
            }
        },
        Step::Minimap { .. } => {},
        Step::WaitSeconds(s) => positive("seconds", *s)?,
        Step::Sample { label } => check_name("label", label)?,
        Step::Screenshot { name } => check_name("name", name)?,
    }
    Ok(())
}

/// Screenshot names become file names inside the output directory, so they
/// must not be able to escape it. Specific reasons are reported for the
/// classic escapes; anything else outside `[A-Za-z0-9_.-]` is refused too.
pub fn check_name(what: &str, name: &str) -> Result<(), String> {
    if name.is_empty() {
        return Err(format!("{what} must not be empty"));
    }
    if name.len() > MAX_NAME_LEN {
        return Err(format!(
            "{what} {name:?} is longer than {MAX_NAME_LEN} bytes"
        ));
    }
    if name.contains('\0') {
        return Err(format!("{what} {name:?} contains a NUL byte"));
    }
    if name.contains('/') || name.contains('\\') {
        return Err(format!("{what} {name:?} contains a path separator"));
    }
    if name.contains("..") {
        return Err(format!("{what} {name:?} contains \"..\""));
    }
    if name.starts_with('.') {
        return Err(format!("{what} {name:?} must not start with '.'"));
    }
    if Path::new(name).is_absolute() || name.contains(':') {
        return Err(format!("{what} {name:?} must be a plain name, not a path"));
    }
    if let Some(c) = name
        .chars()
        .find(|c| !(c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.')))
    {
        return Err(format!(
            "{what} {name:?} contains {c:?}; only ASCII letters, digits, '_', '-' and '.' are \
             allowed"
        ));
    }
    Ok(())
}

/// Finds the source line of the n-th occurrence of a step kind, so
/// validation errors can point at a line. Best effort: comments are skipped,
/// and a kind only matches as a whole identifier followed by `(`.
struct StepLines<'a> {
    src: &'a str,
}

impl<'a> StepLines<'a> {
    fn new(src: &'a str) -> Self { Self { src } }

    fn find(&self, kind: &str, nth: usize) -> Option<usize> {
        let mut count = 0;
        for (line_no, line) in self.src.lines().enumerate() {
            let code = line.split("//").next().unwrap_or("");
            let mut start = 0;
            while let Some(pos) = code[start..].find(kind) {
                let abs = start + pos;
                let before_ok = code[..abs]
                    .chars()
                    .next_back()
                    .is_none_or(|c| !(c.is_ascii_alphanumeric() || c == '_'));
                let after_ok = code[abs + kind.len()..].trim_start().starts_with('(');
                if before_ok && after_ok {
                    if count == nth {
                        return Some(line_no + 1);
                    }
                    count += 1;
                }
                start = abs + kind.len();
            }
        }
        None
    }
}
