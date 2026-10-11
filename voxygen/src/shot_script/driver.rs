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
//! [`TIME_HOURS`]. No text from the script reaches the command path.

use super::schema::{Defaults, ShotScript, Step};
use std::fmt;
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

/// Fixed-precision coordinate formatting (centimetres are plenty).
fn coord(v: f32) -> String { format!("{v:.2}") }

/// What the session must do this frame.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Action {
    SendCommand(ShotCommand),
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
    /// The client's time of day in game seconds, `None` if unknown.
    fn time_of_day(&self) -> Option<f64>;
    /// The client's terrain view distance in chunks, `None` if unknown.
    fn view_distance(&self) -> Option<u32>;
    /// Every chunk within `radius_chunks` of the player is loaded and meshed
    /// (and there is at least one). May stop at the first chunk that is not.
    fn terrain_ready(&self, radius_chunks: u32) -> bool;
    /// Full counts, for failure messages only.
    fn terrain_status(&self, radius_chunks: u32) -> TerrainStatus;
}

/// Record of a finished step.
#[derive(Clone, Debug, PartialEq)]
pub struct StepRecord {
    pub index: usize,
    pub kind: &'static str,
    pub outcome: StepOutcome,
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
        }
    }

    /// Every step has finished (successfully or not).
    pub fn is_finished(&self) -> bool { self.index >= self.steps.len() }

    /// Index of the step currently running.
    pub fn current_index(&self) -> usize { self.index }

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
        let step = self.steps.get(self.index)?.clone();
        let (outcome, action) = if ctx.player_dead() && needs_live_player(&step) {
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
                self.records.push(StepRecord {
                    index: self.index,
                    kind: step.kind(),
                    outcome,
                });
                self.index += 1;
                self.phase = Phase::Start;
            },
        }
        action
    }

    fn tick_step(&mut self, step: &Step, ctx: &impl RunnerContext) -> Tick {
        let now = ctx.now_s();
        match step {
            Step::Time(name) => self.tick_time(name, now, ctx),
            Step::Teleport { wpos } => {
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
            } => self.tick_cromatolis(*px, *py, *z_above_ground, now, ctx),
            Step::WaitForTerrain {
                radius_chunks,
                timeout_s,
            } => {
                let radius = radius_chunks.unwrap_or(self.defaults.terrain_radius_chunks);
                let timeout = timeout_s.unwrap_or(self.defaults.terrain_timeout_s) as f64;
                self.tick_terrain(radius, timeout, now, ctx)
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
            Step::Camera { .. }
            | Step::Viewpoint { .. }
            | Step::Minimap { .. }
            | Step::Sample { .. }
            | Step::Screenshot { .. } => (
                StepOutcome::Failed("step kind not supported by this build yet".into()),
                None,
            ),
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
                (now, radius, None, false, 0)
            },
        };
        let mut last_check = last_check;
        match self.watch_drift(now, ctx) {
            Drift::Ok => {
                if last_check.is_none_or(|t| now - t >= READINESS_INTERVAL_S) {
                    last_check = Some(now);
                    ready = ctx.terrain_ready(radius);
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
            if ctx.terrain_ready(radius) {
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
            let status = ctx.terrain_status(radius);
            (
                StepOutcome::Failed(format!(
                    "terrain not ready after {timeout:.0}s: {} of {} chunks loaded, {} meshed",
                    status.loaded, status.total, status.meshed
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
            } else if now - since <= TELEPORT_TIMEOUT_S {
                // The re-teleport is still travelling to the server and back.
                return Drift::Hold(None);
            } else {
                anchor.returning_since = None;
            }
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
pub fn finished_exit_code(runner: &StepRunner) -> Option<i32> {
    runner
        .any_failed()
        .then_some(super::login::STEP_FAILED_EXIT_CODE)
}

/// Feeds a [`StepRunner`] from the live session.
pub struct SessionDriver {
    runner: StepRunner,
    started: std::time::Instant,
    finished: bool,
}

/// What the session should do after the driver's tick.
#[derive(Debug, PartialEq, Eq)]
pub enum DriverStatus {
    Running,
    /// Every step finished; the session should shut down.
    Finished,
}

impl SessionDriver {
    /// `Some` only while a shot script is active.
    pub fn from_active() -> Option<Self> {
        super::login::active().map(|setup| Self {
            runner: StepRunner::new(&setup.script),
            started: std::time::Instant::now(),
            finished: false,
        })
    }

    /// Run one frame of the script. Waits until the player is in the world.
    pub fn tick(
        &mut self,
        client: &mut client::Client,
        terrain: &crate::scene::terrain::Terrain<common::terrain::TerrainChunk>,
    ) -> DriverStatus {
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
            }
            return DriverStatus::Finished;
        }
        if client.position().is_none() {
            return DriverStatus::Running;
        }
        let ctx = LiveContext {
            client,
            terrain,
            started: self.started,
        };
        if let Some(Action::SendCommand(cmd)) = self.runner.tick(&ctx) {
            info!(command = %cmd, "Shot script command");
            client.send_command(cmd.name.to_owned(), cmd.args);
        }
        DriverStatus::Running
    }
}

struct LiveContext<'a> {
    client: &'a client::Client,
    terrain: &'a crate::scene::terrain::Terrain<common::terrain::TerrainChunk>,
    started: std::time::Instant,
}

impl LiveContext<'_> {
    /// Chunk keys around the player, or `None` while the position is unknown.
    fn keys(&self, radius_chunks: u32) -> Option<impl Iterator<Item = Vec2<i32>>> {
        use common::terrain::CoordinateConversions;
        let pos = self.client.position()?;
        let center: Vec2<i32> = Vec2::<f32>::from(pos)
            .wpos_to_cpos()
            .map(|e| e.floor() as i32);
        let size = self.client.world_data().chunk_size().map(i32::from);
        Some(chunks_in_radius(center, radius_chunks as i32, size))
    }
}

impl RunnerContext for LiveContext<'_> {
    fn now_s(&self) -> f64 { self.started.elapsed().as_secs_f64() }

    fn player_pos(&self) -> Option<Vec3<f32>> { self.client.position() }

    fn player_dead(&self) -> bool { self.client.is_dead() }

    fn time_of_day(&self) -> Option<f64> { Some(self.client.state().get_time_of_day()) }

    fn view_distance(&self) -> Option<u32> { self.client.view_distance() }

    fn terrain_ready(&self, radius_chunks: u32) -> bool {
        let Some(keys) = self.keys(radius_chunks) else {
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

    fn terrain_status(&self, radius_chunks: u32) -> TerrainStatus {
        let mut status = TerrainStatus::default();
        let Some(keys) = self.keys(radius_chunks) else {
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
