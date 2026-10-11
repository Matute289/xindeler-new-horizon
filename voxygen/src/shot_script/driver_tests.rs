use super::{
    ShotScript,
    driver::{
        Action, CROMATOLIS_CLEARANCE, DRIFT_BLOCKS, RunnerContext, ShotCommand, StepOutcome,
        StepRunner, TELEPORT_TIMEOUT_S, TerrainStatus, chunks_in_radius,
    },
};
use std::cell::Cell;
use vek::{Vec2, Vec3};

/// A fake world: the test moves the clock, the player and the terrain.
struct Fake {
    now: Cell<f64>,
    pos: Cell<Option<Vec3<f32>>>,
    terrain: Cell<TerrainStatus>,
    radius_asked: Cell<Option<u32>>,
}

impl Fake {
    fn new() -> Self {
        Self {
            now: Cell::new(0.0),
            pos: Cell::new(Some(Vec3::new(0.0, 0.0, 100.0))),
            terrain: Cell::new(NOT_READY),
            radius_asked: Cell::new(None),
        }
    }

    fn advance(&self, s: f64) { self.now.set(self.now.get() + s); }

    fn at(&self, x: f32, y: f32, z: f32) { self.pos.set(Some(Vec3::new(x, y, z))); }
}

impl RunnerContext for Fake {
    fn now_s(&self) -> f64 { self.now.get() }

    fn player_pos(&self) -> Option<Vec3<f32>> { self.pos.get() }

    fn terrain(&self, radius_chunks: u32) -> TerrainStatus {
        self.radius_asked.set(Some(radius_chunks));
        self.terrain.get()
    }
}

const READY: TerrainStatus = TerrainStatus {
    total: 10,
    loaded: 10,
    meshed: 10,
};
const NOT_READY: TerrainStatus = TerrainStatus {
    total: 10,
    loaded: 10,
    meshed: 7,
};

fn script(steps: &str, settle_frames: u32) -> ShotScript {
    let src = format!(
        "ShotScript(version: 1, window: (1920, 1080), defaults: (view_distance: 12, \
         terrain_radius_chunks: 6, terrain_timeout_s: 60.0, settle_frames: {settle_frames}, \
         hide_hud: true), steps: [{steps}])"
    );
    ShotScript::parse(&src, "test.ron").expect("valid test script")
}

fn line(action: Option<Action>) -> Option<String> {
    action.map(|Action::SendCommand(c)| c.to_string())
}

fn outcomes(r: &StepRunner) -> Vec<StepOutcome> {
    r.records().iter().map(|r| r.outcome.clone()).collect()
}

#[test]
fn time_sends_exact_command_and_finishes() {
    let ctx = Fake::new();
    let mut r = StepRunner::new(&script(r#"Time("noon"), Time("midnight")"#, 0));
    assert_eq!(line(r.tick(&ctx)).as_deref(), Some("/time noon"));
    assert_eq!(line(r.tick(&ctx)).as_deref(), Some("/time midnight"));
    assert!(r.is_finished());
    assert_eq!(r.tick(&ctx), None);
    assert_eq!(outcomes(&r), vec![StepOutcome::Done, StepOutcome::Done]);
}

#[test]
fn command_args_are_separate_and_fixed_precision() {
    let ctx = Fake::new();
    let mut r = StepRunner::new(&script("Teleport(wpos: (16384.0, -2.125, 4150.333))", 0));
    assert_eq!(
        r.tick(&ctx),
        Some(Action::SendCommand(ShotCommand {
            name: "goto",
            args: vec!["16384.00".into(), "-2.12".into(), "4150.33".into()],
        }))
    );
}

#[test]
fn teleport_waits_for_arrival_then_completes() {
    let ctx = Fake::new();
    let mut r = StepRunner::new(&script("Teleport(wpos: (500.0, 600.0, 70.0))", 0));
    assert_eq!(
        line(r.tick(&ctx)).as_deref(),
        Some("/goto 500.00 600.00 70.00")
    );
    ctx.advance(0.1);
    assert_eq!(r.tick(&ctx), None);
    assert!(!r.is_finished());
    // Falling below the target is fine: only horizontal distance counts.
    ctx.at(500.5, 600.0, 40.0);
    assert_eq!(r.tick(&ctx), None);
    assert!(r.is_finished());
    assert_eq!(outcomes(&r), vec![StepOutcome::Done]);
}

#[test]
fn teleport_times_out_when_player_never_arrives() {
    let ctx = Fake::new();
    let mut r = StepRunner::new(&script(
        "Teleport(wpos: (500.0, 600.0, 70.0)), Time(\"noon\")",
        0,
    ));
    r.tick(&ctx);
    ctx.advance(TELEPORT_TIMEOUT_S + 0.1);
    assert_eq!(r.tick(&ctx), None);
    assert!(matches!(outcomes(&r)[0], StepOutcome::Failed(_)));
    // The run continues with the next step.
    assert_eq!(line(r.tick(&ctx)).as_deref(), Some("/time noon"));
}

#[test]
fn cromatolis_teleport_then_height_correction() {
    let ctx = Fake::new();
    let mut r = StepRunner::new(&script(
        "CromatolisTeleport(px: 1024, py: 768, z_above_ground: 2.0)",
        0,
    ));
    assert_eq!(
        line(r.tick(&ctx)).as_deref(),
        Some("/cromatolis_goto 1024 768")
    );
    ctx.advance(0.1);
    assert_eq!(r.tick(&ctx), None, "no move yet");
    // The server put the player CLEARANCE above ground 200.
    ctx.at(16384.0, 16384.0, 200.0 + CROMATOLIS_CLEARANCE);
    assert_eq!(
        line(r.tick(&ctx)).as_deref(),
        Some("/goto 16384.00 16384.00 202.00")
    );
    assert!(!r.is_finished());
    ctx.at(16384.0, 16384.0, 202.0);
    assert_eq!(r.tick(&ctx), None);
    assert_eq!(outcomes(&r), vec![StepOutcome::Done]);
}

#[test]
fn cromatolis_teleport_when_already_there() {
    let ctx = Fake::new();
    ctx.at(10.0, 10.0, 105.0);
    let mut r = StepRunner::new(&script(
        "CromatolisTeleport(px: 1, py: 2, z_above_ground: 5.0)",
        0,
    ));
    r.tick(&ctx);
    ctx.advance(super::driver::CROMATOLIS_SETTLE_S + 0.1);
    assert_eq!(
        line(r.tick(&ctx)).as_deref(),
        Some("/goto 10.00 10.00 105.00")
    );
    r.tick(&ctx);
    assert_eq!(outcomes(&r), vec![StepOutcome::Done]);
}

#[test]
fn wait_for_terrain_needs_loaded_meshed_and_settle_frames() {
    let ctx = Fake::new();
    let mut r = StepRunner::new(&script("WaitForTerrain(radius_chunks: 8)", 3));
    for _ in 0..5 {
        assert_eq!(r.tick(&ctx), None);
        ctx.advance(0.016);
    }
    assert_eq!(ctx.radius_asked.get(), Some(8));
    assert!(!r.is_finished());
    ctx.terrain.set(READY);
    for _ in 0..3 {
        r.tick(&ctx);
        assert!(!r.is_finished());
    }
    // Readiness lost (a new chunk arrived) restarts the settle count.
    ctx.terrain.set(TerrainStatus {
        total: 11,
        loaded: 10,
        meshed: 10,
    });
    r.tick(&ctx);
    ctx.terrain.set(READY);
    for _ in 0..3 {
        r.tick(&ctx);
        assert!(!r.is_finished());
    }
    r.tick(&ctx);
    assert_eq!(outcomes(&r), vec![StepOutcome::Done]);
}

#[test]
fn wait_for_terrain_uses_defaults_and_times_out() {
    let ctx = Fake::new();
    let mut r = StepRunner::new(&script("WaitForTerrain(), WaitSeconds(1.0)", 0));
    r.tick(&ctx);
    assert_eq!(ctx.radius_asked.get(), Some(6));
    ctx.advance(59.0);
    r.tick(&ctx);
    assert!(r.records().is_empty());
    ctx.advance(1.5);
    r.tick(&ctx);
    match &outcomes(&r)[..] {
        [StepOutcome::Failed(reason)] => {
            assert!(reason.contains("7 meshed"), "{reason}");
            assert!(reason.contains("10 of 10 chunks loaded"), "{reason}");
        },
        other => panic!("{other:?}"),
    }
    assert_eq!(r.current_index(), 1);
}

#[test]
fn wait_for_terrain_radius_is_clamped() {
    let ctx = Fake::new();
    let mut r = StepRunner::new(&script("WaitForTerrain(radius_chunks: 100000)", 0));
    r.tick(&ctx);
    assert_eq!(
        ctx.radius_asked.get(),
        Some(super::driver::MAX_RADIUS_CHUNKS)
    );
}

#[test]
fn wait_seconds_uses_the_clock() {
    let ctx = Fake::new();
    let mut r = StepRunner::new(&script("WaitSeconds(2.5)", 0));
    r.tick(&ctx);
    ctx.advance(2.4);
    r.tick(&ctx);
    assert!(!r.is_finished());
    ctx.advance(0.2);
    r.tick(&ctx);
    assert!(r.is_finished());
}

#[test]
fn drift_reteleports_once_then_fails() {
    let ctx = Fake::new();
    let mut r = StepRunner::new(&script(
        "Teleport(wpos: (100.0, 100.0, 50.0)), WaitForTerrain(), WaitSeconds(100.0)",
        0,
    ));
    r.tick(&ctx);
    ctx.at(100.0, 100.0, 50.0);
    r.tick(&ctx);
    assert_eq!(r.current_index(), 1);
    // Pushed away by water: one re-teleport, same target.
    ctx.at(100.0 + DRIFT_BLOCKS + 1.0, 100.0, 45.0);
    assert_eq!(
        line(r.tick(&ctx)).as_deref(),
        Some("/goto 100.00 100.00 50.00")
    );
    // While the re-teleport travels, the step is held even if terrain is ready.
    ctx.terrain.set(READY);
    ctx.advance(0.1);
    assert_eq!(r.tick(&ctx), None);
    assert_eq!(r.current_index(), 1);
    // Back at the target: terrain wait completes.
    ctx.at(100.0, 100.0, 50.0);
    r.tick(&ctx);
    r.tick(&ctx);
    assert_eq!(r.current_index(), 2);
    // Drifting again: no second re-teleport, the step fails.
    ctx.at(110.0, 100.0, 50.0);
    assert_eq!(r.tick(&ctx), None);
    assert!(r.is_finished());
    let o = outcomes(&r);
    assert_eq!(o[0], StepOutcome::Done);
    assert_eq!(o[1], StepOutcome::Done);
    assert!(matches!(&o[2], StepOutcome::Failed(m) if m.contains("drifted")));
}

#[test]
fn reteleport_that_never_arrives_fails_after_timeout() {
    let ctx = Fake::new();
    let mut r = StepRunner::new(&script(
        "Teleport(wpos: (0.0, 0.0, 100.0)), WaitSeconds(1000.0)",
        0,
    ));
    r.tick(&ctx);
    r.tick(&ctx);
    ctx.at(50.0, 0.0, 100.0);
    assert!(line(r.tick(&ctx)).is_some());
    ctx.advance(TELEPORT_TIMEOUT_S / 2.0);
    r.tick(&ctx);
    assert_eq!(r.records().len(), 1);
    ctx.advance(TELEPORT_TIMEOUT_S);
    r.tick(&ctx);
    assert!(matches!(&outcomes(&r)[1], StepOutcome::Failed(_)));
}

#[test]
fn steps_run_strictly_in_order_one_at_a_time() {
    let ctx = Fake::new();
    let mut r = StepRunner::new(&script(
        r#"Time("dawn"), WaitSeconds(1.0), Teleport(wpos: (0.0, 0.0, 100.0)), Time("dusk")"#,
        0,
    ));
    let mut sent = Vec::new();
    for _ in 0..20 {
        if let Some(l) = line(r.tick(&ctx)) {
            sent.push(l);
        }
        ctx.advance(0.5);
    }
    assert_eq!(sent, vec![
        "/time dawn",
        "/goto 0.00 0.00 100.00",
        "/time dusk"
    ]);
    let kinds: Vec<_> = r.records().iter().map(|r| r.kind).collect();
    assert_eq!(kinds, vec!["Time", "WaitSeconds", "Teleport", "Time"]);
}

#[test]
fn later_slice_steps_fail_without_commands() {
    let ctx = Fake::new();
    let mut r = StepRunner::new(&script(r#"Screenshot(name: "a"), Minimap(show: true)"#, 0));
    assert_eq!(r.tick(&ctx), None);
    assert_eq!(r.tick(&ctx), None);
    assert!(
        outcomes(&r)
            .iter()
            .all(|o| matches!(o, StepOutcome::Failed(_)))
    );
}

/// A time name that is not one of the whitelisted names never becomes a
/// command, even if the script bypassed validation.
#[test]
fn no_command_injection_through_names() {
    let ctx = Fake::new();
    let mut s = script(r#"Time("noon")"#, 0);
    s.steps[0] = super::Step::Time("noon /kill".into());
    let mut r = StepRunner::new(&s);
    assert_eq!(r.tick(&ctx), None);
    assert!(matches!(&outcomes(&r)[0], StepOutcome::Failed(_)));

    // Validated names are mapped to the whitelist's own strings.
    assert!(
        ShotScript::parse(
            r#"ShotScript(version: 1, window: (1920, 1080), defaults: (view_distance: 12,
               terrain_radius_chunks: 6, terrain_timeout_s: 60.0, settle_frames: 0,
               hide_hud: true), steps: [Time("noon; /kill")])"#,
            "t.ron"
        )
        .is_err()
    );
}

#[test]
fn chunks_in_radius_is_a_disc_clipped_to_the_world() {
    let all: Vec<_> = chunks_in_radius(Vec2::new(10, 10), 1, Vec2::new(64, 64)).collect();
    assert_eq!(all.len(), 5);
    assert_eq!(
        chunks_in_radius(Vec2::new(10, 10), 6, Vec2::new(64, 64)).count(),
        113
    );
    // At the corner only the in-world quarter counts.
    let corner: Vec<_> = chunks_in_radius(Vec2::new(0, 0), 1, Vec2::new(64, 64)).collect();
    assert_eq!(corner, vec![
        Vec2::new(0, 0),
        Vec2::new(1, 0),
        Vec2::new(0, 1)
    ]);
}

/// The clearance used to recover the ground height must match the server's.
#[test]
fn cromatolis_clearance_matches_server() {
    let server = include_str!("../../../server/src/cmd.rs");
    let line = server
        .lines()
        .find(|l| l.starts_with("const CROMATOLIS_TELEPORT_CLEARANCE"))
        .expect("server clearance constant");
    let value: f32 = line
        .split('=')
        .nth(1)
        .and_then(|v| v.trim().trim_end_matches(';').parse().ok())
        .expect("numeric clearance");
    assert_eq!(value, CROMATOLIS_CLEARANCE);
}

/// The command names must exist with the argument shapes used here.
#[test]
fn command_names_match_common() {
    let common = include_str!("../../../common/src/cmd.rs");
    for name in ["\"goto\"", "\"cromatolis_goto\"", "\"time\""] {
        assert!(
            common.contains(&format!("=> {name}")),
            "{name} missing from the command keyword table"
        );
    }
}
