//! Step executor for shot scripts.
//!
//! [`StepRunner`] is a pure state machine: it is ticked once per frame with a
//! [`RunnerContext`] (clock, player position, terrain readiness) and answers
//! with at most one [`Action`] to perform. It never blocks and never touches
//! the client itself, so it can be tested with a fake context.
//!
//! [`SessionDriver`] is the thin glue that feeds it from the real session.
//!
//! Commands are built only from validated, typed steps: the command name is a
//! `'static` string from this file and every argument is either a number
//! formatted here with fixed precision or a `'static` string from
//! [`TIME_NAMES`]. No text from the script reaches the command path.

use super::schema::{Defaults, ShotScript, Step, TIME_NAMES};
use std::fmt;
use tracing::{info, warn};
use vek::{Vec2, Vec3};

/// Horizontal distance, in blocks, within which the player counts as having
/// arrived at a teleport target, and beyond which (after arriving) the player
/// counts as having drifted away from it. Vertical movement is ignored:
/// falling onto the ground below a teleport target is expected.
pub const DRIFT_BLOCKS: f32 = 2.0;

/// How long a teleport may take to show up in the player's position before
/// the step fails, in seconds.
pub const TELEPORT_TIMEOUT_S: f64 = 30.0;

/// After `/cromatolis_goto`, how long to wait for the player to move before
/// assuming they were already standing at that pixel, in seconds.
pub const CROMATOLIS_SETTLE_S: f64 = 3.0;

/// Height above the ground at which the server's `/cromatolis_goto` places the
/// player (`CROMATOLIS_TELEPORT_CLEARANCE` in `server/src/cmd.rs`; a test keeps
/// them in sync).
pub const CROMATOLIS_CLEARANCE: f32 = 5.0;

/// Largest radius, in chunks, `WaitForTerrain` checks. Bounds the per-frame
/// cost of the readiness scan; larger requests are clamped (and logged).
pub const MAX_RADIUS_CHUNKS: u32 = 64;

/// Horizontal movement, in blocks, that counts as "the cromatolis teleport
/// has happened".
const CROMATOLIS_MOVED_BLOCKS: f32 = 1.0;

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

    fn cromatolis_goto(px: u32, py: u32) -> Self {
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

/// Terrain readiness around the player.
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
    pub fn ready(&self) -> bool { self.loaded == self.total && self.meshed == self.total }
}

/// What the runner needs to know about the world each frame.
pub trait RunnerContext {
    /// Monotonic time in seconds.
    fn now_s(&self) -> f64;
    /// Player position, `None` while unknown.
    fn player_pos(&self) -> Option<Vec3<f32>>;
    /// Readiness of the chunks within `radius_chunks` of the player.
    fn terrain(&self, radius_chunks: u32) -> TerrainStatus;
}

/// Record of a finished step.
#[derive(Clone, Debug, PartialEq)]
pub struct StepRecord {
    pub index: usize,
    pub kind: &'static str,
    pub outcome: StepOutcome,
}

/// The last teleport target, watched for drift by the waiting steps.
#[derive(Clone, Debug)]
struct Anchor {
    target: Vec3<f32>,
    /// The single re-teleport has been used.
    regoto_used: bool,
    /// When the re-teleport was sent, until the player is back at the target.
    returning_since: Option<f64>,
}

/// Per-step state, reset when a step starts.
#[derive(Clone, Debug)]
enum Phase {
    Start,
    /// Waiting for the player to reach the horizontal target.
    Arriving {
        since: f64,
    },
    /// `/cromatolis_goto` sent; waiting for the player to move.
    CromatolisMoving {
        since: f64,
        from: Option<Vec3<f32>>,
    },
    /// Waiting until `since + seconds`.
    Waiting {
        since: f64,
    },
    /// Terrain wait: `ready_frames` consecutive ready frames so far.
    Terrain {
        since: f64,
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

    /// Advance the current step by one frame.
    pub fn tick(&mut self, ctx: &impl RunnerContext) -> Option<Action> {
        let step = self.steps.get(self.index)?.clone();
        let (outcome, action) = self.tick_step(&step, ctx);
        match outcome {
            StepOutcome::Waiting => {},
            outcome => {
                if let StepOutcome::Failed(reason) = &outcome {
                    report_step_failure(self.index, step.kind(), reason);
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

    fn tick_step(
        &mut self,
        step: &Step,
        ctx: &impl RunnerContext,
    ) -> (StepOutcome, Option<Action>) {
        let now = ctx.now_s();
        match step {
            Step::Time(name) => match TIME_NAMES.iter().find(|t| **t == name.as_str()) {
                Some(t) => (
                    StepOutcome::Done,
                    Some(Action::SendCommand(ShotCommand::time(t))),
                ),
                // Unreachable for a validated script.
                None => (StepOutcome::Failed("unknown time of day".into()), None),
            },
            Step::Teleport { wpos } => {
                let target = Vec3::new(wpos.0, wpos.1, wpos.2);
                self.tick_teleport(target, now, ctx)
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
                    _ => {
                        self.phase = Phase::Waiting { since: now };
                        now
                    },
                };
                if let Some(r) = self.watch_drift(now, ctx) {
                    return r;
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

    /// Send `/goto`, then wait until the player is horizontally at the target.
    fn tick_teleport(
        &mut self,
        target: Vec3<f32>,
        now: f64,
        ctx: &impl RunnerContext,
    ) -> (StepOutcome, Option<Action>) {
        match self.phase {
            Phase::Start => {
                self.anchor = Some(Anchor {
                    target,
                    regoto_used: false,
                    returning_since: None,
                });
                self.phase = Phase::Arriving { since: now };
                (
                    StepOutcome::Waiting,
                    Some(Action::SendCommand(ShotCommand::goto(target))),
                )
            },
            Phase::Arriving { since } => {
                if ctx
                    .player_pos()
                    .is_some_and(|p| horizontal_distance(p, target) <= DRIFT_BLOCKS)
                {
                    (StepOutcome::Done, None)
                } else if now - since > TELEPORT_TIMEOUT_S {
                    (
                        StepOutcome::Failed(format!(
                            "player did not reach the teleport target within \
                             {TELEPORT_TIMEOUT_S:.0}s (position {})",
                            fmt_pos(ctx.player_pos())
                        )),
                        None,
                    )
                } else {
                    (StepOutcome::Waiting, None)
                }
            },
            _ => (
                StepOutcome::Failed("internal: unexpected phase".into()),
                None,
            ),
        }
    }

    /// Send `/cromatolis_goto`, wait for the player to land there, then
    /// correct the height to `z_above_ground` with a `/goto`.
    fn tick_cromatolis(
        &mut self,
        px: u32,
        py: u32,
        z_above_ground: f32,
        now: f64,
        ctx: &impl RunnerContext,
    ) -> (StepOutcome, Option<Action>) {
        match self.phase {
            Phase::Start => {
                self.phase = Phase::CromatolisMoving {
                    since: now,
                    from: ctx.player_pos(),
                };
                (
                    StepOutcome::Waiting,
                    Some(Action::SendCommand(ShotCommand::cromatolis_goto(px, py))),
                )
            },
            Phase::CromatolisMoving { since, from } => {
                let Some(pos) = ctx.player_pos() else {
                    return if now - since > TELEPORT_TIMEOUT_S {
                        (StepOutcome::Failed("player position unknown".into()), None)
                    } else {
                        (StepOutcome::Waiting, None)
                    };
                };
                let moved =
                    from.is_none_or(|f| horizontal_distance(f, pos) > CROMATOLIS_MOVED_BLOCKS);
                if !moved && now - since < CROMATOLIS_SETTLE_S {
                    return (StepOutcome::Waiting, None);
                }
                // After a jump the server has just put the player
                // `CROMATOLIS_CLEARANCE` above the ground; without one the
                // player was already standing there. Re-place them at the
                // requested height above that ground.
                let ground = if moved {
                    pos.z - CROMATOLIS_CLEARANCE
                } else {
                    pos.z
                };
                let target = Vec3::new(pos.x, pos.y, ground + z_above_ground);
                self.anchor = Some(Anchor {
                    target,
                    regoto_used: false,
                    returning_since: None,
                });
                self.phase = Phase::Arriving { since: now };
                (
                    StepOutcome::Waiting,
                    Some(Action::SendCommand(ShotCommand::goto(target))),
                )
            },
            Phase::Arriving { .. } => {
                let target = self.anchor.as_ref().map_or(Vec3::zero(), |a| a.target);
                self.tick_teleport(target, now, ctx)
            },
            _ => (
                StepOutcome::Failed("internal: unexpected phase".into()),
                None,
            ),
        }
    }

    /// Wait until every chunk within `radius` is loaded and meshed, then for
    /// `settle_frames` more ready frames.
    fn tick_terrain(
        &mut self,
        radius: u32,
        timeout: f64,
        now: f64,
        ctx: &impl RunnerContext,
    ) -> (StepOutcome, Option<Action>) {
        let (since, ready_frames) = match self.phase {
            Phase::Terrain {
                since,
                ready_frames,
            } => (since, ready_frames),
            _ => {
                if radius > MAX_RADIUS_CHUNKS {
                    warn!(
                        radius,
                        max = MAX_RADIUS_CHUNKS,
                        "WaitForTerrain radius clamped"
                    );
                }
                (now, 0)
            },
        };
        if let Some(r) = self.watch_drift(now, ctx) {
            // A re-teleport (or waiting for it) restarts the settle count, so
            // the area around the target is what gets checked.
            self.phase = Phase::Terrain {
                since,
                ready_frames: 0,
            };
            return r;
        }
        let status = ctx.terrain(radius.min(MAX_RADIUS_CHUNKS));
        let ready_frames = if status.ready() {
            ready_frames.saturating_add(1)
        } else {
            0
        };
        self.phase = Phase::Terrain {
            since,
            ready_frames,
        };
        if ready_frames > self.defaults.settle_frames {
            (StepOutcome::Done, None)
        } else if now - since > timeout {
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
    /// target; fail the step if they drift again afterwards.
    fn watch_drift(
        &mut self,
        now: f64,
        ctx: &impl RunnerContext,
    ) -> Option<(StepOutcome, Option<Action>)> {
        let anchor = self.anchor.as_mut()?;
        let pos = ctx.player_pos()?;
        let d = horizontal_distance(pos, anchor.target);
        if d <= DRIFT_BLOCKS {
            anchor.returning_since = None;
            return None;
        }
        // The re-teleport is still travelling to the server and back; hold
        // the step until the player is back.
        if let Some(since) = anchor.returning_since {
            if now - since <= TELEPORT_TIMEOUT_S {
                return Some((StepOutcome::Waiting, None));
            }
            anchor.returning_since = None;
        }
        if anchor.regoto_used {
            let target = anchor.target;
            // Report once, then stop watching this anchor.
            self.anchor = None;
            return Some((
                StepOutcome::Failed(format!(
                    "player drifted {d:.1} blocks from the teleport target {} after a re-teleport",
                    fmt_pos(Some(target))
                )),
                None,
            ));
        }
        anchor.regoto_used = true;
        anchor.returning_since = Some(now);
        warn!(
            drift = d,
            "Player drifted from the teleport target; teleporting again"
        );
        Some((
            StepOutcome::Waiting,
            Some(Action::SendCommand(ShotCommand::goto(anchor.target))),
        ))
    }
}

fn horizontal_distance(a: Vec3<f32>, b: Vec3<f32>) -> f32 { Vec2::from(a).distance(Vec2::from(b)) }

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

/// Feeds a [`StepRunner`] from the live session.
pub struct SessionDriver {
    runner: StepRunner,
    started: std::time::Instant,
    finished_logged: bool,
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
            finished_logged: false,
        })
    }

    /// Run one frame of the script. Waits until the player is in the world.
    pub fn tick(
        &mut self,
        client: &mut client::Client,
        terrain: &crate::scene::terrain::Terrain<common::terrain::TerrainChunk>,
    ) -> DriverStatus {
        if self.runner.is_finished() {
            if !self.finished_logged {
                self.finished_logged = true;
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

impl RunnerContext for LiveContext<'_> {
    fn now_s(&self) -> f64 { self.started.elapsed().as_secs_f64() }

    fn player_pos(&self) -> Option<Vec3<f32>> { self.client.position() }

    fn terrain(&self, radius_chunks: u32) -> TerrainStatus {
        use common::terrain::CoordinateConversions;
        let Some(pos) = self.client.position() else {
            return TerrainStatus::default();
        };
        let center: Vec2<i32> = Vec2::<f32>::from(pos)
            .wpos_to_cpos()
            .map(|e| e.floor() as i32);
        let size = self.client.world_data().chunk_size().map(i32::from);
        let client_terrain = self.client.state().terrain();
        let r = radius_chunks as i32;
        let mut status = TerrainStatus::default();
        for key in chunks_in_radius(center, r, size) {
            status.total += 1;
            if client_terrain.get_key(key).is_some() {
                status.loaded += 1;
            }
            if self.terrain.is_meshed(key) {
                status.meshed += 1;
            }
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
