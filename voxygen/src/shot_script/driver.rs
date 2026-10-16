//! Step executor for shot scripts.
//!
//! [`StepRunner`] is a pure state machine: it is ticked once per frame with a
//! [`RunnerContext`] (clock, player state, time of day, terrain readiness) and
//! answers with at most one [`Action`] to perform. It never blocks and never
//! touches the client itself, so it can be tested with a fake context.
//!
//! [`SessionDriver`] is the thin glue that feeds it from the real session.
//!
//! Commands are built only from validated, typed steps: the command name is a
//! `'static` string from this file and every argument is either a number
//! formatted here with fixed precision or a `'static` string from
//! [`TIME_HOURS`]. No text from the script reaches the command path. The
//! camera, minimap, HUD and screenshot steps are not commands at all: they
//! become typed [`Action`]s that the session applies directly, and a
//! screenshot file name is built by [`screenshot_file_name`], which only ever
//! emits `[A-Za-z0-9_.-]`.

use super::{
    capture::ShotResult,
    schema::{Defaults, ShotScript, Step},
};
use std::{fmt, path::PathBuf};
use tracing::{info, warn};
use vek::{Vec2, Vec3};

/// Horizontal distance, in blocks, within which the player counts as being at
/// a teleport target, and beyond which (once there) as having drifted.
pub const DRIFT_BLOCKS: f32 = 2.0;

/// Vertical tolerance, in blocks, for arriving at a teleport target. `/goto`
/// places the player exactly at the target height and gravity takes over from
/// there; 3 blocks is a fall of roughly 0.75 s from rest, so a frame or a
/// network round trip of delay still counts as arrived, while a command the
/// server refused (the player stays where they were) does not.
pub const ARRIVE_Z_BLOCKS: f32 = 3.0;

/// Smallest movement in a single frame, in blocks, that counts as the server
/// teleporting the player. Walking, knockback and normal falls move the player
/// far less than this per frame; a teleport is a discontinuous jump.
pub const TELEPORT_JUMP_BLOCKS: f32 = 3.0;

/// Movement per frame, in blocks, below which the player counts as at rest.
pub const REST_EPS_BLOCKS: f32 = 0.05;

/// Consecutive frames at rest before the player counts as settled.
pub const REST_FRAMES: u32 = 10;

/// How long a teleport may take to show up and settle, in seconds.
pub const TELEPORT_TIMEOUT_S: f64 = 30.0;

/// How long to wait for `/cromatolis_goto` to move the player, in seconds.
pub const CROMATOLIS_MOVE_TIMEOUT_S: f64 = 10.0;

/// How long to wait for `/time` to show up in the client's time of day.
pub const TIME_TIMEOUT_S: f64 = 10.0;

/// Accepted distance from the requested time of day, in game seconds. Game
/// time runs much faster than real time, so this leaves room for the delay
/// between the server applying the command and the client seeing it.
pub const TIME_TOLERANCE_S: f64 = 1800.0;

/// Frames the HUD gets to disappear before a screenshot is requested.
pub const HUD_HIDE_FRAMES: u32 = 3;

/// Frames a camera change gets before the next step runs.
pub const CAMERA_MIN_FRAMES: u32 = 2;

/// Longest a camera change waits for the zoom to settle, in seconds. The
/// camera pulls in on obstacles, so the requested distance may never be
/// reached; the step then simply continues.
pub const CAMERA_SETTLE_MAX_S: f64 = 3.0;

/// How long to wait for the screenshot file to be written, in seconds.
pub const SCREENSHOT_TIMEOUT_S: f64 = 30.0;

/// Times a screenshot request is repeated when the renderer dropped it.
pub const SCREENSHOT_RETRIES: u32 = 3;

/// Time between terrain readiness checks, in seconds.
pub const READINESS_INTERVAL_S: f64 = 0.25;

/// Largest radius, in chunks, `WaitForTerrain` checks. Bounds the cost of the
/// readiness scan; larger requests are clamped (and logged).
pub const MAX_RADIUS_CHUNKS: u32 = 64;

const DAY_S: f64 = 86_400.0;

/// The named times `/time` accepts and the hour each one sets (from
/// `handle_time` in `server/src/cmd.rs`; a test keeps them in sync).
pub const TIME_HOURS: &[(&str, u32)] = &[
    ("midnight", 0),
    ("night", 20),
    ("dawn", 5),
    ("morning", 8),
    ("day", 10),
    ("noon", 12),
    ("dusk", 17),
];

/// A whitelisted admin command, ready for `Client::send_command`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ShotCommand {
    pub name: &'static str,
    pub args: Vec<String>,
}

impl ShotCommand {
    fn time(name: &'static str) -> Self {
        Self {
            name: "time",
            args: vec![name.to_owned()],
        }
    }

    fn goto(pos: Vec3<f32>) -> Self {
        Self {
            name: "goto",
            args: vec![coord(pos.x), coord(pos.y), coord(pos.z)],
        }
    }

    fn cromatolis_goto(px: i32, py: i32) -> Self {
        Self {
            name: "cromatolis_goto",
            args: vec![px.to_string(), py.to_string()],
        }
    }
}

impl fmt::Display for ShotCommand {
    /// The command as it would be typed in chat, e.g. `/goto 1.00 2.00 3.00`.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "/{}", self.name)?;
        for a in &self.args {
            write!(f, " {a}")?;
        }
        Ok(())
    }
}

/// Decide how the HUD comes back when a step that hid it ends: in this tick
/// when it has no other action, otherwise (an action cannot be replaced)
/// flagged for the next tick.
fn settle_hud(hidden: bool, action: Option<Action>, pending: &mut bool) -> Option<Action> {
    match (hidden, action) {
        (true, None) => Some(Action::Hud(true)),
        (true, Some(a)) => {
            *pending = true;
            Some(a)
        },
        (false, a) => a,
    }
}

/// Fixed-precision coordinate formatting (centimetres are plenty).
fn coord(v: f32) -> String { format!("{v:.2}") }

/// Name of a screenshot file: `NNN_<name>.png`, where `NNN` is the 1-based
/// ordinal (at least three digits so files sort correctly up to 999 shots) of
/// the screenshot in the script. Anything outside `[A-Za-z0-9_.-]` becomes `_`
/// and a leading `.` is replaced too, so the result is always a single plain
/// file name (validated names already are; this is the second line of defence).
pub fn screenshot_file_name(ordinal: u32, name: &str) -> String {
    let clean: String = name
        .chars()
        .enumerate()
        .map(|(i, c)| match c {
            'A'..='Z' | 'a'..='z' | '0'..='9' | '_' | '-' => c,
            '.' if i > 0 => c,
            _ => '_',
        })
        .collect();
    // `..` can never appear, even inside a name.
    let clean = clean.replace("..", "_");
    format!("{ordinal:03}_{clean}.png")
}

/// Orbit-camera orientation for a `Camera` step, as the camera's
/// `(yaw, pitch)` in radians. The script's yaw is the compass heading of the
/// view direction (0 looks along +Y, 90 along +X); the script's pitch is the
/// elevation of the view direction (negative looks down), which is the
/// opposite sign of the camera's own pitch.
pub fn camera_orientation(yaw_deg: f32, pitch_deg: f32) -> (f32, f32) {
    (yaw_deg.to_radians(), -pitch_deg.to_radians())
}

/// Camera `(yaw, pitch)` in radians for a camera at `cam` looking at
/// `look_at`, in the same convention as [`camera_orientation`]'s result.
/// `None` when the two points coincide.
pub fn look_at_orientation(cam: Vec3<f32>, look_at: Vec3<f32>) -> Option<(f32, f32)> {
    let d = look_at - cam;
    let len = d.magnitude();
    if !(len.is_finite() && len > f32::EPSILON) {
        return None;
    }
    let f = d / len;
    Some((f.x.atan2(f.y), (-f.z).clamp(-1.0, 1.0).asin()))
}

/// What the session must do this frame.
#[derive(Clone, Debug, PartialEq)]
pub enum Action {
    SendCommand(ShotCommand),
    /// Orbit camera around the player.
    Camera {
        yaw_rad: f32,
        pitch_rad: f32,
        distance: f32,
    },
    /// Free-flying camera at `cam` looking along `(yaw_rad, pitch_rad)`.
    Viewpoint {
        cam: Vec3<f32>,
        yaw_rad: f32,
        pitch_rad: f32,
    },
    Minimap(bool),
    /// Show (`true`) or hide (`false`) the whole HUD.
    Hud(bool),
    /// Write the next frame to `<out dir>/<file_name>`.
    Screenshot {
        file_name: String,
    },
}

/// How a screenshot request ended.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ShotOutcome {
    Written,
    /// The renderer discarded the request; asking again is fine.
    Dropped,
    Failed(String),
}

/// Result of ticking the current step.
#[derive(Clone, Debug, PartialEq)]
pub enum StepOutcome {
    Done,
    Waiting,
    Failed(String),
}

/// Terrain readiness around the player, for failure messages.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct TerrainStatus {
    /// Chunks inside the radius (and inside the world).
    pub total: usize,
    /// Of those, chunks the client has received.
    pub loaded: usize,
    /// Of those, chunks with a finished mesh and no pending remesh.
    pub meshed: usize,
}

impl TerrainStatus {
    /// Every chunk is loaded and meshed, and there is at least one.
    pub fn ready(&self) -> bool {
        self.total > 0 && self.loaded == self.total && self.meshed == self.total
    }
}

/// What the runner needs to know about the world.
pub trait RunnerContext {
    /// Monotonic time in seconds.
    fn now_s(&self) -> f64;
    /// Player position, `None` while unknown.
    fn player_pos(&self) -> Option<Vec3<f32>>;
    /// The player is dead (waiting to respawn).
    fn player_dead(&self) -> bool;
    /// The player is in liquid. A swimmer never comes to rest (buoyancy and
    /// sinking move them every frame), so the drift watch does not apply.
    fn player_in_liquid(&self) -> bool { false }
    /// The client's time of day in game seconds, `None` if unknown.
    fn time_of_day(&self) -> Option<f64>;
    /// The client's terrain view distance in chunks, `None` if unknown.
    fn view_distance(&self) -> Option<u32>;
    /// Every chunk within `radius_chunks` of `center` (the player when
    /// `None`; world x/y) is loaded and meshed (and there is at least one).
    /// May stop at the first chunk that is not.
    fn terrain_ready(&self, center: Option<Vec2<f32>>, radius_chunks: u32) -> bool;
    /// Full counts, for failure messages only.
    fn terrain_status(&self, center: Option<Vec2<f32>>, radius_chunks: u32) -> TerrainStatus;
    /// The camera finished its zoom (distance and field of view).
    fn camera_settled(&self) -> bool;
    /// The result of the outstanding screenshot request, once known.
    fn screenshot_result(&self) -> Option<ShotOutcome>;
}

/// Record of a finished step.
#[derive(Clone, Debug, PartialEq)]
pub struct StepRecord {
    pub index: usize,
    pub kind: &'static str,
    pub outcome: StepOutcome,
    /// Runner clock (seconds) when the step first ran.
    pub start_s: f64,
    /// Runner clock (seconds) when the step ended.
    pub end_s: f64,
    /// Player position when the step started, if known.
    pub pos_before: Option<Vec3<f32>>,
    /// Player position when the step ended, if known.
    pub pos_after: Option<Vec3<f32>>,
    /// Terrain readiness where the step waited for terrain, with whether
    /// that wait timed out.
    pub terrain: Option<(TerrainStatus, bool)>,
    /// File name (not path) of the screenshot the step requested.
    pub screenshot: Option<String>,
}

/// What is known about the step that is running.
#[derive(Clone, Debug)]
struct StepMeta {
    start_s: f64,
    pos_before: Option<Vec3<f32>>,
    terrain: Option<(TerrainStatus, bool)>,
    screenshot: Option<String>,
}

/// Tracks whether the player has come to rest.
#[derive(Clone, Copy, Debug, Default)]
struct Rest {
    last: Option<Vec3<f32>>,
    frames: u32,
}

impl Rest {
    /// Feed this frame's position; returns the movement since the last frame.
    fn update(&mut self, pos: Vec3<f32>) -> f32 {
        let moved = self.last.map_or(0.0, |l| l.distance(pos));
        if self.last.is_some() && moved < REST_EPS_BLOCKS {
            self.frames = self.frames.saturating_add(1);
        } else {
            self.frames = 0;
        }
        self.last = Some(pos);
        moved
    }

    fn settled(&self) -> bool { self.frames >= REST_FRAMES }
}

/// The last teleport target, watched for drift by the waiting steps.
#[derive(Clone, Debug)]
struct Anchor {
    target: Vec3<f32>,
    /// The single re-teleport has been used.
    regoto_used: bool,
    /// When the re-teleport was sent, until the player is back at the target.
    returning_since: Option<f64>,
    rest: Rest,
    /// Height where the player came to rest after the teleport; vertical
    /// drift is measured from here (falling to it is expected).
    rest_z: Option<f32>,
}

impl Anchor {
    fn new(target: Vec3<f32>) -> Self {
        Self {
            target,
            regoto_used: false,
            returning_since: None,
            rest: Rest::default(),
            rest_z: None,
        }
    }
}

/// Result of the drift check.
enum Drift {
    /// At the target (or nothing to watch).
    Ok,
    /// The step must not progress this frame (re-teleport sent or in flight).
    Hold(Option<Action>),
    Failed(String),
}

/// Per-step state, reset when a step starts.
#[derive(Clone, Debug)]
enum Phase {
    Start,
    /// `/time` sent at `since`.
    TimeWait {
        since: f64,
        hour: u32,
    },
    /// `/goto` sent; waiting for the player to reach the target.
    Arriving {
        since: f64,
        from: Vec3<f32>,
    },
    /// `/cromatolis_goto` sent; waiting for the jump.
    CromatolisMoving {
        since: f64,
        rest: Rest,
    },
    /// Jumped; waiting for the player to come to rest.
    CromatolisLanding {
        since: f64,
        rest: Rest,
    },
    /// Waiting since `since`.
    Waiting {
        since: f64,
    },
    /// Camera change applied; letting it settle.
    CameraSettle {
        since: f64,
        frames: u32,
    },
    /// Viewpoint applied; waiting for terrain around the camera.
    ViewpointSent,
    /// HUD hidden for a screenshot; letting it disappear.
    HudHiding {
        frames: u32,
    },
    /// Screenshot requested at `since`.
    ShotWait {
        since: f64,
        attempts: u32,
    },
    /// Terrain wait.
    Terrain {
        since: f64,
        radius: u32,
        last_check: Option<f64>,
        ready: bool,
        ready_frames: u32,
    },
}

/// The step state machine.
pub struct StepRunner {
    steps: Vec<Step>,
    defaults: Defaults,
    index: usize,
    phase: Phase,
    anchor: Option<Anchor>,
    records: Vec<StepRecord>,
    /// World x/y the camera looks at after a `Viewpoint`; `None` while the
    /// camera follows the player. Set by `Viewpoint` and cleared only by the
    /// `Camera` step that restores the third-person camera: teleporting the
    /// player does not move a free camera.
    view_center: Option<Vec2<f32>>,
    /// The HUD must be shown again on the next tick (the step that hid it
    /// ended on a tick that already had another action to emit).
    pending_hud_restore: bool,
    /// Screenshots started so far (names the next file).
    shots_started: u32,
    shot: ShotState,
    /// A screenshot could not be written.
    screenshot_failed: bool,
    /// Bookkeeping for the report, for the step that is running.
    meta: Option<StepMeta>,
}

/// State of the screenshot step that is running.
#[derive(Clone, Debug, Default)]
struct ShotState {
    /// The HUD was hidden for this shot and must be shown again.
    hud_hidden: bool,
    /// Why the terrain was not ready; the shot is still taken, then flagged.
    soft_failure: Option<String>,
}

type Tick = (StepOutcome, Option<Action>);

impl StepRunner {
    pub fn new(script: &ShotScript) -> Self {
        Self {
            steps: script.steps.clone(),
            defaults: script.defaults.clone(),
            index: 0,
            phase: Phase::Start,
            anchor: None,
            records: Vec::new(),
            view_center: None,
            pending_hud_restore: false,
            shots_started: 0,
            shot: ShotState::default(),
            screenshot_failed: false,
            meta: None,
        }
    }

    /// Every step of the script, in order (finished or not).
    pub fn steps(&self) -> &[Step] { &self.steps }

    /// The script's defaults.
    pub fn defaults(&self) -> &Defaults { &self.defaults }

    /// A screenshot could not be written (read-back or file error).
    pub fn screenshot_failed(&self) -> bool { self.screenshot_failed }

    /// Every step has finished (successfully or not).
    pub fn is_finished(&self) -> bool { self.index >= self.steps.len() }

    /// Index of the step currently running.
    pub fn current_index(&self) -> usize { self.index }

    /// Index and start time (driver clock) of the step that is running now:
    /// `None` before the first tick, between steps and once finished.
    pub fn in_flight(&self) -> Option<(usize, f64)> {
        if self.is_finished() {
            return None;
        }
        self.meta.as_ref().map(|m| (self.index, m.start_s))
    }

    /// Finished steps, in order.
    pub fn records(&self) -> &[StepRecord] { &self.records }

    /// At least one finished step failed.
    pub fn any_failed(&self) -> bool {
        self.records
            .iter()
            .any(|r| matches!(r.outcome, StepOutcome::Failed(_)))
    }

    /// Advance the current step by one frame.
    pub fn tick(&mut self, ctx: &impl RunnerContext) -> Option<Action> {
        if std::mem::take(&mut self.pending_hud_restore) {
            return Some(Action::Hud(true));
        }
        let step = self.steps.get(self.index)?.clone();
        if self.meta.is_none() {
            self.meta = Some(StepMeta {
                start_s: ctx.now_s(),
                pos_before: ctx.player_pos(),
                terrain: None,
                screenshot: None,
            });
        }
        let (outcome, mut action) = if ctx.player_dead() && needs_live_player(&step) {
            (StepOutcome::Failed("the player is dead".into()), None)
        } else {
            self.tick_step(&step, ctx)
        };
        match outcome {
            StepOutcome::Waiting => {},
            outcome => {
                if let StepOutcome::Failed(reason) = &outcome {
                    report_step_failure(self.index, step.kind(), reason);
                    // A failed teleport (or drift) leaves no target to watch.
                    if matches!(
                        step,
                        Step::Teleport { .. } | Step::CromatolisTeleport { .. }
                    ) || ctx.player_dead()
                    {
                        self.anchor = None;
                    }
                } else {
                    info!(step = self.index + 1, kind = step.kind(), "Shot step done");
                }
                let meta = self.meta.take().unwrap_or(StepMeta {
                    start_s: ctx.now_s(),
                    pos_before: None,
                    terrain: None,
                    screenshot: None,
                });
                self.records.push(StepRecord {
                    index: self.index,
                    kind: step.kind(),
                    outcome,
                    start_s: meta.start_s,
                    end_s: ctx.now_s(),
                    pos_before: meta.pos_before,
                    pos_after: ctx.player_pos(),
                    terrain: meta.terrain,
                    screenshot: meta.screenshot,
                });
                self.index += 1;
                self.phase = Phase::Start;
                // Whatever ended the step, the HUD comes back.
                action = settle_hud(self.shot.hud_hidden, action, &mut self.pending_hud_restore);
                self.shot = ShotState::default();
            },
        }
        action
    }

    fn tick_step(&mut self, step: &Step, ctx: &impl RunnerContext) -> Tick {
        let now = ctx.now_s();
        match step {
            Step::Time(name) => self.tick_time(name, now, ctx),
            Step::Teleport { wpos } => {
                // `view_center` is deliberately kept: after a `Viewpoint` the
                // lens stays at the viewpoint (the camera is still free)
                // until a `Camera` step puts it back behind the player, and
                // terrain waits and screenshots must watch where the lens is.
                let target = Vec3::new(wpos.0, wpos.1, wpos.2);
                match self.phase {
                    Phase::Start => self.start_goto(target, now, ctx),
                    _ => self.tick_arriving(now, ctx),
                }
            },
            Step::CromatolisTeleport {
                px,
                py,
                z_above_ground,
            } => {
                // `view_center` is kept, see `Teleport`.
                self.tick_cromatolis(*px, *py, *z_above_ground, now, ctx)
            },
            Step::WaitForTerrain {
                radius_chunks,
                timeout_s,
            } => {
                let radius = radius_chunks.unwrap_or(self.defaults.terrain_radius_chunks);
                let timeout = timeout_s.unwrap_or(self.defaults.terrain_timeout_s) as f64;
                let center = self.view_center;
                self.tick_terrain(center, radius, timeout, now, ctx)
            },
            Step::WaitSeconds(s) => {
                let since = match self.phase {
                    Phase::Waiting { since } => since,
                    _ => now,
                };
                self.phase = Phase::Waiting { since };
                match self.watch_drift(now, ctx) {
                    Drift::Ok => {},
                    Drift::Hold(action) => {
                        // Restart the wait once the player is back.
                        self.phase = Phase::Waiting { since: now };
                        return (StepOutcome::Waiting, action);
                    },
                    Drift::Failed(reason) => return (StepOutcome::Failed(reason), None),
                }
                if now - since >= *s as f64 {
                    (StepOutcome::Done, None)
                } else {
                    (StepOutcome::Waiting, None)
                }
            },
            Step::Camera {
                yaw_deg,
                pitch_deg,
                distance,
            } => self.tick_camera(*yaw_deg, *pitch_deg, *distance, now, ctx),
            Step::Viewpoint { cam, look_at } => self.tick_viewpoint(*cam, *look_at, now, ctx),
            Step::Minimap { show } => (StepOutcome::Done, Some(Action::Minimap(*show))),
            Step::Sample { label } => {
                info!(label = %label, "Shot sample marker");
                (StepOutcome::Done, None)
            },
            Step::Screenshot { name } => self.tick_screenshot(name, now, ctx),
        }
    }

    /// Put the camera back behind the player at the given orbit, then give
    /// the zoom a moment to settle.
    fn tick_camera(
        &mut self,
        yaw_deg: f32,
        pitch_deg: f32,
        distance: f32,
        now: f64,
        ctx: &impl RunnerContext,
    ) -> Tick {
        match self.phase {
            Phase::CameraSettle { since, frames } => {
                let frames = frames.saturating_add(1);
                if (frames >= CAMERA_MIN_FRAMES && ctx.camera_settled())
                    || now - since >= CAMERA_SETTLE_MAX_S
                {
                    (StepOutcome::Done, None)
                } else {
                    self.phase = Phase::CameraSettle { since, frames };
                    (StepOutcome::Waiting, None)
                }
            },
            _ => {
                self.view_center = None;
                self.phase = Phase::CameraSettle {
                    since: now,
                    frames: 0,
                };
                let (yaw_rad, pitch_rad) = camera_orientation(yaw_deg, pitch_deg);
                (
                    StepOutcome::Waiting,
                    Some(Action::Camera {
                        yaw_rad,
                        pitch_rad,
                        distance,
                    }),
                )
            },
        }
    }

    /// Move the free camera to `cam`, then wait until the terrain around it
    /// is loaded and meshed.
    fn tick_viewpoint(
        &mut self,
        cam: (f32, f32, f32),
        look_at: (f32, f32, f32),
        now: f64,
        ctx: &impl RunnerContext,
    ) -> Tick {
        let cam = Vec3::new(cam.0, cam.1, cam.2);
        match self.phase {
            Phase::ViewpointSent | Phase::Terrain { .. } => {
                let radius = self.defaults.terrain_radius_chunks;
                let timeout = self.defaults.terrain_timeout_s as f64;
                let center = Some(cam.xy());
                self.tick_terrain(center, radius, timeout, now, ctx)
            },
            _ => {
                let Some((yaw_rad, pitch_rad)) = look_at_orientation(cam, Vec3::from(look_at))
                else {
                    return (StepOutcome::Failed("cam and look_at coincide".into()), None);
                };
                self.view_center = Some(cam.xy());
                self.phase = Phase::ViewpointSent;
                (
                    StepOutcome::Waiting,
                    Some(Action::Viewpoint {
                        cam,
                        yaw_rad,
                        pitch_rad,
                    }),
                )
            },
        }
    }

    /// Hide the HUD (if the script asks), wait for terrain to be ready and
    /// settled, request the capture and wait for the file. A terrain timeout
    /// still takes the shot and is reported afterwards.
    fn tick_screenshot(&mut self, name: &str, now: f64, ctx: &impl RunnerContext) -> Tick {
        let _ = name;
        let center = self.view_center;
        let radius = self.defaults.terrain_radius_chunks;
        let timeout = self.defaults.terrain_timeout_s as f64;
        match self.phase {
            Phase::Start => {
                self.shots_started += 1;
                if self.defaults.hide_hud {
                    self.shot.hud_hidden = true;
                    self.phase = Phase::HudHiding { frames: 0 };
                    (StepOutcome::Waiting, Some(Action::Hud(false)))
                } else {
                    self.shot_terrain(name, center, radius, timeout, now, ctx)
                }
            },
            Phase::HudHiding { frames } => {
                if frames + 1 >= HUD_HIDE_FRAMES {
                    self.phase = Phase::Start;
                    self.shot_terrain(name, center, radius, timeout, now, ctx)
                } else {
                    self.phase = Phase::HudHiding { frames: frames + 1 };
                    (StepOutcome::Waiting, None)
                }
            },
            Phase::ShotWait { since, attempts } => match ctx.screenshot_result() {
                Some(ShotOutcome::Written) => match self.shot.soft_failure.take() {
                    Some(reason) => (StepOutcome::Failed(reason), None),
                    None => (StepOutcome::Done, None),
                },
                Some(ShotOutcome::Dropped) if attempts < SCREENSHOT_RETRIES => {
                    self.phase = Phase::ShotWait {
                        since: now,
                        attempts: attempts + 1,
                    };
                    (StepOutcome::Waiting, Some(self.shot_action(name)))
                },
                Some(ShotOutcome::Dropped) => {
                    self.screenshot_failed = true;
                    (
                        StepOutcome::Failed(format!(
                            "the renderer discarded the screenshot request {} times",
                            attempts + 1
                        )),
                        None,
                    )
                },
                Some(ShotOutcome::Failed(reason)) => {
                    self.screenshot_failed = true;
                    (StepOutcome::Failed(reason), None)
                },
                None if now - since > SCREENSHOT_TIMEOUT_S => {
                    self.screenshot_failed = true;
                    (
                        StepOutcome::Failed(format!(
                            "screenshot was not written within {SCREENSHOT_TIMEOUT_S:.0}s"
                        )),
                        None,
                    )
                },
                None => (StepOutcome::Waiting, None),
            },
            _ => self.shot_terrain(name, center, radius, timeout, now, ctx),
        }
    }

    /// Terrain wait of a screenshot step; once it ends (ready or timed out)
    /// the capture is requested.
    fn shot_terrain(
        &mut self,
        name: &str,
        center: Option<Vec2<f32>>,
        radius: u32,
        timeout: f64,
        now: f64,
        ctx: &impl RunnerContext,
    ) -> Tick {
        let (outcome, action) = self.tick_terrain(center, radius, timeout, now, ctx);
        match outcome {
            StepOutcome::Waiting => (StepOutcome::Waiting, action),
            ended => {
                if let StepOutcome::Failed(reason) = ended {
                    warn!(
                        reason,
                        "Terrain not ready for a screenshot; taking it anyway"
                    );
                    self.shot.soft_failure = Some(reason);
                }
                self.phase = Phase::ShotWait {
                    since: now,
                    attempts: 0,
                };
                (StepOutcome::Waiting, Some(self.shot_action(name)))
            },
        }
    }

    fn shot_action(&mut self, name: &str) -> Action {
        let file_name = screenshot_file_name(self.shots_started, name);
        if let Some(meta) = &mut self.meta {
            meta.screenshot = Some(file_name.clone());
        }
        Action::Screenshot { file_name }
    }

    /// Remember the terrain readiness the running step ended with.
    fn note_terrain(&mut self, status: TerrainStatus, timed_out: bool) {
        if let Some(meta) = &mut self.meta {
            meta.terrain = Some((status, timed_out));
        }
    }

    /// Send `/time`, then wait until the client's time of day shows it.
    fn tick_time(&mut self, name: &str, now: f64, ctx: &impl RunnerContext) -> Tick {
        match self.phase {
            Phase::TimeWait { since, hour } => match ctx.time_of_day() {
                // Not observable: nothing to wait for.
                None => (StepOutcome::Done, None),
                Some(tod) if day_distance(tod, hour as f64 * 3600.0) <= TIME_TOLERANCE_S => {
                    (StepOutcome::Done, None)
                },
                Some(tod) if now - since > TIME_TIMEOUT_S => (
                    StepOutcome::Failed(format!(
                        "time of day did not change to {name} within {TIME_TIMEOUT_S:.0}s (now \
                         {:.0}s into the day)",
                        tod.rem_euclid(DAY_S)
                    )),
                    None,
                ),
                Some(_) => (StepOutcome::Waiting, None),
            },
            _ => match TIME_HOURS.iter().find(|(t, _)| *t == name) {
                Some((t, hour)) => {
                    self.phase = Phase::TimeWait {
                        since: now,
                        hour: *hour,
                    };
                    (
                        StepOutcome::Waiting,
                        Some(Action::SendCommand(ShotCommand::time(t))),
                    )
                },
                // Unreachable for a validated script.
                None => (StepOutcome::Failed("unknown time of day".into()), None),
            },
        }
    }

    /// Start a `/goto` to `target`, unless the player is already there.
    fn start_goto(&mut self, target: Vec3<f32>, now: f64, ctx: &impl RunnerContext) -> Tick {
        let Some(pos) = ctx.player_pos() else {
            return (StepOutcome::Waiting, None);
        };
        self.anchor = Some(Anchor::new(target));
        if at_target(pos, target) {
            return (StepOutcome::Done, None);
        }
        self.phase = Phase::Arriving {
            since: now,
            from: pos,
        };
        (
            StepOutcome::Waiting,
            Some(Action::SendCommand(ShotCommand::goto(target))),
        )
    }

    /// Wait until the player has moved and is at the anchor's target.
    fn tick_arriving(&mut self, now: f64, ctx: &impl RunnerContext) -> Tick {
        let (Phase::Arriving { since, from }, Some(anchor)) = (&self.phase, &self.anchor) else {
            return (
                StepOutcome::Failed("internal: unexpected phase".into()),
                None,
            );
        };
        let target = anchor.target;
        let pos = ctx.player_pos();
        if let Some(pos) = pos
            && pos != *from
            && at_target(pos, target)
        {
            (StepOutcome::Done, None)
        } else if now - since > TELEPORT_TIMEOUT_S {
            (
                StepOutcome::Failed(format!(
                    "player did not reach the teleport target {} within {TELEPORT_TIMEOUT_S:.0}s \
                     (position {}); was the command refused?",
                    fmt_pos(Some(target)),
                    fmt_pos(pos)
                )),
                None,
            )
        } else {
            (StepOutcome::Waiting, None)
        }
    }

    /// Send `/cromatolis_goto`, wait for the jump and for the player to come
    /// to rest, then `/goto` to `z_above_ground` above where they rest.
    ///
    /// The server places the player a fixed clearance above the world
    /// generator's smooth surface height at that column, which is not the
    /// voxel surface (rock, trees and buildings can be above or below it). So
    /// the ground is taken from where the player actually comes to rest.
    fn tick_cromatolis(
        &mut self,
        px: u32,
        py: u32,
        z_above_ground: f32,
        now: f64,
        ctx: &impl RunnerContext,
    ) -> Tick {
        match self.phase {
            Phase::Start => {
                // The server parses the pixel as `i32`.
                let (Ok(px), Ok(py)) = (i32::try_from(px), i32::try_from(py)) else {
                    return (
                        StepOutcome::Failed(format!("map pixel ({px}, {py}) is out of range")),
                        None,
                    );
                };
                let Some(pos) = ctx.player_pos() else {
                    return (StepOutcome::Waiting, None);
                };
                let mut rest = Rest::default();
                rest.update(pos);
                self.phase = Phase::CromatolisMoving { since: now, rest };
                (
                    StepOutcome::Waiting,
                    Some(Action::SendCommand(ShotCommand::cromatolis_goto(px, py))),
                )
            },
            Phase::CromatolisMoving { since, mut rest } => {
                if let Some(pos) = ctx.player_pos()
                    && rest.update(pos) >= TELEPORT_JUMP_BLOCKS
                {
                    self.phase = Phase::CromatolisLanding {
                        since: now,
                        rest: Rest {
                            last: Some(pos),
                            frames: 0,
                        },
                    };
                    return (StepOutcome::Waiting, None);
                }
                self.phase = Phase::CromatolisMoving { since, rest };
                if now - since > CROMATOLIS_MOVE_TIMEOUT_S {
                    (
                        StepOutcome::Failed(format!(
                            "the player was not moved within {CROMATOLIS_MOVE_TIMEOUT_S:.0}s: the \
                             command was refused (no admin rights?) or the pixel is outside the \
                             map (position {})",
                            fmt_pos(ctx.player_pos())
                        )),
                        None,
                    )
                } else {
                    (StepOutcome::Waiting, None)
                }
            },
            Phase::CromatolisLanding { since, mut rest } => {
                if let Some(pos) = ctx.player_pos() {
                    rest.update(pos);
                    if rest.settled() {
                        let target = Vec3::new(pos.x, pos.y, pos.z + z_above_ground);
                        return self.start_goto(target, now, ctx);
                    }
                }
                self.phase = Phase::CromatolisLanding { since, rest };
                if now - since > TELEPORT_TIMEOUT_S {
                    (
                        StepOutcome::Failed(format!(
                            "the player did not come to rest within {TELEPORT_TIMEOUT_S:.0}s \
                             after the teleport (position {})",
                            fmt_pos(ctx.player_pos())
                        )),
                        None,
                    )
                } else {
                    (StepOutcome::Waiting, None)
                }
            },
            Phase::Arriving { .. } => self.tick_arriving(now, ctx),
            _ => (
                StepOutcome::Failed("internal: unexpected phase".into()),
                None,
            ),
        }
    }

    /// Wait until every chunk within `radius` is loaded and meshed, then for
    /// `settle_frames` more frames, re-checked at the end.
    fn tick_terrain(
        &mut self,
        center: Option<Vec2<f32>>,
        radius: u32,
        timeout: f64,
        now: f64,
        ctx: &impl RunnerContext,
    ) -> Tick {
        let (since, radius, last_check, mut ready, mut ready_frames) = match self.phase {
            Phase::Terrain {
                since,
                radius,
                last_check,
                ready,
                ready_frames,
            } => (since, radius, last_check, ready, ready_frames),
            _ => {
                if radius > MAX_RADIUS_CHUNKS {
                    warn!(
                        radius,
                        max = MAX_RADIUS_CHUNKS,
                        "WaitForTerrain radius clamped"
                    );
                }
                let radius = radius.min(MAX_RADIUS_CHUNKS);
                // Chunks are meshed only once all their neighbours are
                // loaded, so the radius must stay inside the view distance.
                if let Some(vd) = ctx.view_distance()
                    && radius >= vd
                {
                    return (
                        StepOutcome::Failed(format!(
                            "radius {radius} chunks can never be ready with a view distance of \
                             {vd} chunks (the radius must be smaller than the view distance)"
                        )),
                        None,
                    );
                }
                // The server only sends chunks within the view distance of
                // the player, so a lens farther away than that can never see
                // its whole disc loaded (typically a `Viewpoint` followed by
                // a teleport without a `Camera` step): fail at once instead
                // of waiting out the timeout.
                if let (Some(vd), Some(c), Some(p)) =
                    (ctx.view_distance(), center, ctx.player_pos())
                {
                    use common::vol::RectVolSize;
                    let chunk = common::terrain::TerrainChunkSize::RECT_SIZE.x as f32;
                    let lens_chunks = Vec2::<f32>::from(p).distance(c) / chunk;
                    if lens_chunks + radius as f32 > vd as f32 + 1.0 {
                        return (
                            StepOutcome::Failed(format!(
                                "the camera is {lens_chunks:.1} chunks from the player, so a \
                                 radius of {radius} chunks can never be loaded within the view \
                                 distance of {vd} chunks: move the player near the lens, or put a \
                                 Camera step after the teleport to bring the camera back"
                            )),
                            None,
                        );
                    }
                }
                (now, radius, None, false, 0)
            },
        };
        let mut last_check = last_check;
        match self.watch_drift(now, ctx) {
            Drift::Ok => {
                if last_check.is_none_or(|t| now - t >= READINESS_INTERVAL_S) {
                    last_check = Some(now);
                    ready = ctx.terrain_ready(center, radius);
                    if !ready {
                        ready_frames = 0;
                    }
                }
                if ready {
                    ready_frames = ready_frames.saturating_add(1);
                }
            },
            Drift::Hold(action) => {
                // The settle count restarts once the player is back.
                self.phase = Phase::Terrain {
                    since,
                    radius,
                    last_check: None,
                    ready: false,
                    ready_frames: 0,
                };
                return (StepOutcome::Waiting, action);
            },
            Drift::Failed(reason) => return (StepOutcome::Failed(reason), None),
        }
        if ready && ready_frames > self.defaults.settle_frames {
            // Confirm right before finishing: chunks may have changed since
            // the last throttled check.
            if ctx.terrain_ready(center, radius) {
                let status = ctx.terrain_status(center, radius);
                self.note_terrain(status, false);
                return (StepOutcome::Done, None);
            }
            ready = false;
            ready_frames = 0;
            last_check = Some(now);
        }
        self.phase = Phase::Terrain {
            since,
            radius,
            last_check,
            ready,
            ready_frames,
        };
        if now - since > timeout {
            let status = ctx.terrain_status(center, radius);
            self.note_terrain(status, true);
            (
                StepOutcome::Failed(format!(
                    "terrain not ready after {timeout:.0}s: {} of {} chunks loaded, {} meshed{}",
                    status.loaded,
                    status.total,
                    status.meshed,
                    if center.is_some() {
                        " (the viewpoint may be farther than the server view distance from the \
                         player: teleport the player near it first)"
                    } else {
                        ""
                    }
                )),
                None,
            )
        } else {
            (StepOutcome::Waiting, None)
        }
    }

    /// Re-teleport once if the player drifted away from the last teleport
    /// target (horizontally, or vertically from where they came to rest);
    /// fail if they drift again afterwards.
    fn watch_drift(&mut self, now: f64, ctx: &impl RunnerContext) -> Drift {
        let Some(anchor) = self.anchor.as_mut() else {
            return Drift::Ok;
        };
        let Some(pos) = ctx.player_pos() else {
            return Drift::Ok;
        };
        let target = anchor.target;
        let horizontal = horizontal_distance(pos, target);

        if let Some(since) = anchor.returning_since {
            if at_target(pos, target) {
                anchor.returning_since = None;
                anchor.rest = Rest::default();
                anchor.rest_z = None;
                // Back: the waiting step restarts from now.
                return Drift::Hold(None);
            } else if now - since <= TELEPORT_TIMEOUT_S {
                // The re-teleport is still travelling to the server and back.
                return Drift::Hold(None);
            } else {
                anchor.returning_since = None;
            }
        }

        if ctx.player_in_liquid() {
            // Swimming: the player floats up or sinks slowly and never
            // rests, which is not a drift from the target.
            anchor.rest = Rest::default();
            anchor.rest_z = None;
            return Drift::Ok;
        }
        anchor.rest.update(pos);
        if anchor.rest_z.is_none() && anchor.rest.settled() {
            anchor.rest_z = Some(pos.z);
        }
        let vertical = anchor.rest_z.map_or(0.0, |z| (pos.z - z).abs());
        if horizontal <= DRIFT_BLOCKS && vertical <= DRIFT_BLOCKS {
            return Drift::Ok;
        }
        if anchor.regoto_used {
            // Report once, then stop watching this anchor.
            self.anchor = None;
            return Drift::Failed(format!(
                "player drifted from the teleport target {} to {} after a re-teleport",
                fmt_pos(Some(target)),
                fmt_pos(Some(pos))
            ));
        }
        anchor.regoto_used = true;
        anchor.returning_since = Some(now);
        warn!(
            horizontal,
            vertical, "Player drifted from the teleport target; teleporting again"
        );
        Drift::Hold(Some(Action::SendCommand(ShotCommand::goto(target))))
    }
}

/// Steps that cannot make progress while the player is dead.
fn needs_live_player(step: &Step) -> bool {
    matches!(
        step,
        Step::Teleport { .. }
            | Step::CromatolisTeleport { .. }
            | Step::WaitForTerrain { .. }
            | Step::WaitSeconds(_)
    )
}

fn at_target(pos: Vec3<f32>, target: Vec3<f32>) -> bool {
    horizontal_distance(pos, target) <= DRIFT_BLOCKS && (pos.z - target.z).abs() <= ARRIVE_Z_BLOCKS
}

fn horizontal_distance(a: Vec3<f32>, b: Vec3<f32>) -> f32 { Vec2::from(a).distance(Vec2::from(b)) }

/// Distance between two times of day, in seconds, around the day cycle.
fn day_distance(tod: f64, target_s: f64) -> f64 {
    let d = (tod - target_s).rem_euclid(DAY_S);
    d.min(DAY_S - d)
}

fn fmt_pos(p: Option<Vec3<f32>>) -> String {
    match p {
        Some(p) => format!("({:.1}, {:.1}, {:.1})", p.x, p.y, p.z),
        None => "unknown".into(),
    }
}

/// Single exit point for step failures, so the report can collect them.
/// For now the failure is only logged; the run continues with the next step.
pub fn report_step_failure(index: usize, kind: &str, reason: &str) {
    warn!(step = index + 1, kind, reason, "Shot step failed");
}

/// Process exit code for a finished run: `None` when every step succeeded.
///
/// A screenshot that could not be written outranks plain step failures.
pub fn finished_exit_code(runner: &StepRunner) -> Option<i32> {
    if runner.screenshot_failed() {
        Some(super::login::SCREENSHOT_FAILED_EXIT_CODE)
    } else if runner.any_failed() {
        Some(super::login::STEP_FAILED_EXIT_CODE)
    } else {
        None
    }
}

/// Exit code of a run that ends now, whether or not the script reached its
/// end: the finished-run code if any, `0` for a completed script, and
/// [`STEP_FAILED_EXIT_CODE`](super::login::STEP_FAILED_EXIT_CODE) for one cut
/// short. The process exit code and the report both use this.
pub fn abort_code(runner: &StepRunner) -> i32 {
    finished_exit_code(runner).unwrap_or(if runner.is_finished() {
        0
    } else {
        super::login::STEP_FAILED_EXIT_CODE
    })
}

/// Why a run that ends now was cut short; `None` for a completed script.
pub fn abort_reason(runner: &StepRunner) -> Option<String> {
    if runner.is_finished() {
        return None;
    }
    let index = runner.current_index();
    Some(match runner.steps().get(index) {
        Some(step) => format!(
            "the session ended before the script finished (at step index {index}, {})",
            step.kind()
        ),
        None => "the session ended before the script finished".to_owned(),
    })
}

/// Exit code recorded for a panic (Rust's default for a panicking process).
pub const PANIC_EXIT_CODE: i32 = 101;

/// Feeds a [`StepRunner`] from the live session.
pub struct SessionDriver {
    runner: StepRunner,
    started: std::time::Instant,
    finished: bool,
    out_dir: PathBuf,
    shot_tx: crossbeam_channel::Sender<ShotResult>,
    shot_rx: crossbeam_channel::Receiver<ShotResult>,
    /// File the outstanding screenshot request writes.
    expected_shot: Option<PathBuf>,
    /// The report was written (the run is over).
    report_written: bool,
    /// The client's view distance, last seen.
    last_view_distance: Option<u32>,
}

/// What the session should do after the driver's tick.
#[derive(Debug, PartialEq, Eq)]
pub enum DriverStatus {
    Running,
    /// Every step finished; the session should shut down.
    Finished,
}

/// A change the session applies to its scene, HUD or window.
#[derive(Debug)]
pub enum SessionEffect {
    Camera {
        yaw_rad: f32,
        pitch_rad: f32,
        distance: f32,
    },
    Viewpoint {
        cam: Vec3<f32>,
        yaw_rad: f32,
        pitch_rad: f32,
    },
    Minimap(bool),
    Hud(bool),
    Screenshot {
        path: PathBuf,
        done: crossbeam_channel::Sender<ShotResult>,
    },
}

/// Inputs the session supplies on top of the client.
#[derive(Clone, Copy, Debug)]
pub struct SceneState {
    /// The camera finished its zoom.
    pub camera_settled: bool,
}

impl SessionDriver {
    /// `Some` only while a shot script is active.
    pub fn from_active() -> Option<Self> {
        super::login::active().map(|setup| {
            // Room for every result that can be outstanding (one at a time).
            let (shot_tx, shot_rx) = crossbeam_channel::bounded(4);
            Self {
                runner: StepRunner::new(&setup.script),
                started: std::time::Instant::now(),
                finished: false,
                out_dir: setup.out_dir.clone(),
                shot_tx,
                shot_rx,
                expected_shot: None,
                report_written: false,
                last_view_distance: None,
            }
        })
    }

    /// Run one frame of the script. Waits until the player is in the world.
    /// The returned effect, if any, must be applied by the session before
    /// the next frame.
    pub fn tick(
        &mut self,
        client: &mut client::Client,
        terrain: &crate::scene::terrain::Terrain<common::terrain::TerrainChunk>,
        scene: SceneState,
    ) -> (DriverStatus, Option<SessionEffect>) {
        if self.runner.is_finished() {
            if !self.finished {
                self.finished = true;
                let failed = self
                    .runner
                    .records()
                    .iter()
                    .filter(|r| matches!(r.outcome, StepOutcome::Failed(_)))
                    .count();
                info!(
                    steps = self.runner.records().len(),
                    failed, "Shot script finished"
                );
                if let Some(code) = finished_exit_code(&self.runner) {
                    super::login::set_exit_code(code);
                }
                self.write_report(None, None);
            }
            return (DriverStatus::Finished, None);
        }
        if client.position().is_none() {
            return (DriverStatus::Running, None);
        }
        self.last_view_distance = client.view_distance();
        let screenshot =
            take_matching_result(&self.shot_rx, self.expected_shot.as_deref()).map(|r| match r {
                Ok(path) => {
                    info!(path = %path.display(), "Shot screenshot written");
                    ShotOutcome::Written
                },
                Err(super::capture::ShotError::Dropped { .. }) => ShotOutcome::Dropped,
                Err(e) => ShotOutcome::Failed(e.to_string()),
            });
        let ctx = LiveContext {
            client,
            terrain,
            started: self.started,
            camera_settled: scene.camera_settled,
            screenshot,
        };
        let effect = match self.runner.tick(&ctx) {
            None => None,
            Some(Action::SendCommand(cmd)) => {
                info!(command = %cmd, "Shot script command");
                client.send_command(cmd.name.to_owned(), cmd.args);
                None
            },
            Some(Action::Camera {
                yaw_rad,
                pitch_rad,
                distance,
            }) => Some(SessionEffect::Camera {
                yaw_rad,
                pitch_rad,
                distance,
            }),
            Some(Action::Viewpoint {
                cam,
                yaw_rad,
                pitch_rad,
            }) => Some(SessionEffect::Viewpoint {
                cam,
                yaw_rad,
                pitch_rad,
            }),
            Some(Action::Minimap(show)) => Some(SessionEffect::Minimap(show)),
            Some(Action::Hud(show)) => Some(SessionEffect::Hud(show)),
            Some(Action::Screenshot { file_name }) => {
                info!(file = %file_name, "Shot screenshot requested");
                let path = self.out_dir.join(file_name);
                // A late result of an earlier (timed out) request must not
                // be taken for this one.
                while self.shot_rx.try_recv().is_ok() {}
                self.expected_shot = Some(path.clone());
                Some(SessionEffect::Screenshot {
                    path,
                    done: self.shot_tx.clone(),
                })
            },
        };
        (DriverStatus::Running, effect)
    }
}

impl SessionDriver {
    /// Write `report.json`. `abort_reason` is `Some` when the script did not
    /// run to its end. Written once.
    fn write_report(&mut self, abort_reason: Option<String>, exit_code: Option<i32>) {
        use super::report;
        if std::mem::replace(&mut self.report_written, true) {
            return;
        }
        let Some(setup) = super::login::active() else {
            return;
        };
        let info = report::run_info(
            setup,
            self.last_view_distance,
            abort_reason,
            super::login::STEP_FAILED_EXIT_CODE,
        );
        let out_dir = self.out_dir.clone();
        let mut report = report::build_report(&self.runner, &info, &|file| {
            report::file_facts(&out_dir, file)
        });
        if let Some(code) = exit_code {
            report.exit_code = code;
        }
        report::log_write(&self.out_dir, &report);
    }
}

impl Drop for SessionDriver {
    /// The session ended before the report was written (window closed,
    /// connection lost, or a panic unwinding): the run still gets a report,
    /// with the same exit code the process ends with. Never panics.
    fn drop(&mut self) {
        if self.report_written {
            return;
        }
        let panicking = std::thread::panicking();
        let (reason, code) = if panicking {
            ("panic".to_owned(), Some(PANIC_EXIT_CODE))
        } else {
            (abort_reason(&self.runner).unwrap_or_default(), None)
        };
        let reason = (!reason.is_empty()).then_some(reason);
        let exit = code.unwrap_or_else(|| abort_code(&self.runner));
        if exit != 0 {
            super::login::set_exit_code(exit);
        }
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            self.write_report(reason, Some(exit));
        }));
        drop(result);
    }
}

/// Next screenshot result that belongs to `expected`; results for other
/// files (late answers to abandoned requests) are discarded.
fn take_matching_result(
    rx: &crossbeam_channel::Receiver<ShotResult>,
    expected: Option<&std::path::Path>,
) -> Option<ShotResult> {
    use super::capture::ShotError;
    while let Ok(r) = rx.try_recv() {
        let path = match &r {
            Ok(p) => Some(p.as_path()),
            Err(
                ShotError::InvalidPath { path, .. }
                | ShotError::Write { path, .. }
                | ShotError::Dropped { path },
            ) => Some(path.as_path()),
            Err(ShotError::Readback(_)) => None,
        };
        if path.is_none_or(|p| Some(p) == expected) {
            return Some(r);
        }
        warn!(path = ?path, "Ignoring a stale screenshot result");
    }
    None
}

struct LiveContext<'a> {
    client: &'a client::Client,
    terrain: &'a crate::scene::terrain::Terrain<common::terrain::TerrainChunk>,
    started: std::time::Instant,
    camera_settled: bool,
    screenshot: Option<ShotOutcome>,
}

impl LiveContext<'_> {
    /// Chunk keys around `center` (the player by default), or `None` while
    /// the position is unknown.
    fn keys(
        &self,
        center: Option<Vec2<f32>>,
        radius_chunks: u32,
    ) -> Option<impl Iterator<Item = Vec2<i32>>> {
        use common::terrain::CoordinateConversions;
        let center = match center {
            Some(c) => c,
            None => Vec2::<f32>::from(self.client.position()?),
        };
        let center: Vec2<i32> = center.wpos_to_cpos().map(|e| e.floor() as i32);
        let size = self.client.world_data().chunk_size().map(i32::from);
        Some(chunks_in_radius(center, radius_chunks as i32, size))
    }
}

impl RunnerContext for LiveContext<'_> {
    fn now_s(&self) -> f64 { self.started.elapsed().as_secs_f64() }

    fn player_pos(&self) -> Option<Vec3<f32>> { self.client.position() }

    fn player_dead(&self) -> bool { self.client.is_dead() }

    fn player_in_liquid(&self) -> bool {
        self.client
            .current::<common::comp::PhysicsState>()
            .is_some_and(|p| p.in_liquid().is_some())
    }

    fn time_of_day(&self) -> Option<f64> { Some(self.client.state().get_time_of_day()) }

    fn view_distance(&self) -> Option<u32> { self.client.view_distance() }

    fn terrain_ready(&self, center: Option<Vec2<f32>>, radius_chunks: u32) -> bool {
        let Some(keys) = self.keys(center, radius_chunks) else {
            return false;
        };
        let client_terrain = self.client.state().terrain();
        let mut any = false;
        for key in keys {
            if client_terrain.get_key(key).is_none() || !self.terrain.is_meshed(key) {
                return false;
            }
            any = true;
        }
        any
    }

    fn terrain_status(&self, center: Option<Vec2<f32>>, radius_chunks: u32) -> TerrainStatus {
        let mut status = TerrainStatus::default();
        let Some(keys) = self.keys(center, radius_chunks) else {
            return status;
        };
        let client_terrain = self.client.state().terrain();
        for key in keys {
            status.total += 1;
            status.loaded += usize::from(client_terrain.get_key(key).is_some());
            status.meshed += usize::from(self.terrain.is_meshed(key));
        }
        status
    }

    fn camera_settled(&self) -> bool { self.camera_settled }

    fn screenshot_result(&self) -> Option<ShotOutcome> { self.screenshot.clone() }
}

/// Chunk keys within `radius` (a disc) of `center`, inside a world of `size`
/// chunks.
pub fn chunks_in_radius(
    center: Vec2<i32>,
    radius: i32,
    size: Vec2<i32>,
) -> impl Iterator<Item = Vec2<i32>> {
    (-radius..=radius)
        .flat_map(move |dy| (-radius..=radius).map(move |dx| Vec2::new(dx, dy)))
        .filter(move |d| d.x * d.x + d.y * d.y <= radius * radius)
        .map(move |d| center + d)
        .filter(move |k| k.x >= 0 && k.y >= 0 && k.x < size.x && k.y < size.y)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shot_script::capture::ShotError;

    #[test]
    fn hud_comes_back_next_tick_when_the_ending_tick_has_an_action() {
        let mut pending = false;
        assert_eq!(
            settle_hud(true, None, &mut pending),
            Some(Action::Hud(true))
        );
        assert!(!pending);
        let a = Some(Action::Minimap(true));
        assert_eq!(settle_hud(true, a.clone(), &mut pending), a);
        assert!(pending, "restore is kept for the next tick");
        let mut pending = false;
        assert_eq!(settle_hud(false, None, &mut pending), None);
        assert!(!pending);
    }

    #[test]
    fn stale_screenshot_results_are_skipped() {
        let (tx, rx) = crossbeam_channel::unbounded::<ShotResult>();
        let old = PathBuf::from("/o/001_a.png");
        let new = PathBuf::from("/o/002_b.png");
        tx.send(Ok(old.clone())).unwrap();
        tx.send(Err(ShotError::Dropped { path: old })).unwrap();
        assert!(take_matching_result(&rx, Some(&new)).is_none());
        tx.send(Ok(PathBuf::from("/o/001_a.png"))).unwrap();
        tx.send(Ok(new.clone())).unwrap();
        assert_eq!(take_matching_result(&rx, Some(&new)), Some(Ok(new)));
    }
}
