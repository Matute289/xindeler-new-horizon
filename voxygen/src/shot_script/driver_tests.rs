use super::{
    ShotScript,
    driver::{
        Action, CROMATOLIS_MOVE_TIMEOUT_S, DRIFT_BLOCKS, HUD_HIDE_FRAMES, MAX_RADIUS_CHUNKS,
        REST_FRAMES, RunnerContext, SCREENSHOT_RETRIES, SCREENSHOT_TIMEOUT_S, ShotCommand,
        ShotOutcome, StepOutcome, StepRunner, TELEPORT_TIMEOUT_S, TIME_HOURS, TIME_TIMEOUT_S,
        TerrainStatus, camera_orientation, chunks_in_radius, finished_exit_code,
        look_at_orientation, screenshot_file_name,
    },
    login::{SCREENSHOT_FAILED_EXIT_CODE, SETUP_EXIT_CODE, STEP_FAILED_EXIT_CODE},
    schema::TIME_NAMES,
};
use std::cell::{Cell, RefCell};
use vek::{Vec2, Vec3};

/// A fake world: the test moves the clock, the player and the terrain.
struct Fake {
    now: Cell<f64>,
    pos: Cell<Option<Vec3<f32>>>,
    dead: Cell<bool>,
    in_liquid: Cell<bool>,
    tod: Cell<Option<f64>>,
    view_distance: Cell<Option<u32>>,
    terrain: Cell<TerrainStatus>,
    radius_asked: Cell<Option<u32>>,
    ready_checks: Cell<u32>,
    center_asked: Cell<Option<Option<Vec2<f32>>>>,
    camera_settled: Cell<bool>,
    shot: RefCell<Option<ShotOutcome>>,
}

impl Fake {
    fn new() -> Self {
        Self {
            now: Cell::new(0.0),
            pos: Cell::new(Some(Vec3::new(0.0, 0.0, 100.0))),
            dead: Cell::new(false),
            in_liquid: Cell::new(false),
            tod: Cell::new(None),
            view_distance: Cell::new(Some(12)),
            terrain: Cell::new(NOT_READY),
            radius_asked: Cell::new(None),
            ready_checks: Cell::new(0),
            center_asked: Cell::new(None),
            camera_settled: Cell::new(true),
            shot: RefCell::new(None),
        }
    }

    fn advance(&self, s: f64) { self.now.set(self.now.get() + s); }

    fn at(&self, x: f32, y: f32, z: f32) { self.pos.set(Some(Vec3::new(x, y, z))); }
}

impl RunnerContext for Fake {
    fn now_s(&self) -> f64 { self.now.get() }

    fn player_pos(&self) -> Option<Vec3<f32>> { self.pos.get() }

    fn player_dead(&self) -> bool { self.dead.get() }

    fn player_in_liquid(&self) -> bool { self.in_liquid.get() }

    fn time_of_day(&self) -> Option<f64> { self.tod.get() }

    fn view_distance(&self) -> Option<u32> { self.view_distance.get() }

    fn terrain_ready(&self, center: Option<Vec2<f32>>, radius_chunks: u32) -> bool {
        self.center_asked.set(Some(center));
        self.radius_asked.set(Some(radius_chunks));
        self.ready_checks.set(self.ready_checks.get() + 1);
        self.terrain.get().ready()
    }

    fn terrain_status(&self, _center: Option<Vec2<f32>>, _radius: u32) -> TerrainStatus {
        self.terrain.get()
    }

    fn camera_settled(&self) -> bool { self.camera_settled.get() }

    fn screenshot_result(&self) -> Option<ShotOutcome> { self.shot.borrow().clone() }
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
    action.map(|a| match a {
        Action::SendCommand(c) => c.to_string(),
        other => format!("{other:?}"),
    })
}

fn outcomes(r: &StepRunner) -> Vec<StepOutcome> {
    r.records().iter().map(|r| r.outcome.clone()).collect()
}

fn failed_with(o: &StepOutcome, needle: &str) -> bool {
    matches!(o, StepOutcome::Failed(m) if m.contains(needle))
}

/// Tick `n` frames of `dt` seconds without moving anything.
fn frames(r: &mut StepRunner, ctx: &Fake, n: u32, dt: f64) -> Vec<String> {
    let mut sent = Vec::new();
    for _ in 0..n {
        sent.extend(line(r.tick(ctx)));
        ctx.advance(dt);
    }
    sent
}

#[test]
fn time_waits_for_the_time_of_day() {
    let ctx = Fake::new();
    ctx.tod.set(Some(3.0 * 3600.0));
    let mut r = StepRunner::new(&script(r#"Time("noon"), Time("midnight")"#, 0));
    assert_eq!(line(r.tick(&ctx)).as_deref(), Some("/time noon"));
    ctx.advance(0.1);
    assert_eq!(r.tick(&ctx), None);
    assert!(r.records().is_empty(), "waits for the server");
    // Next day, noon.
    ctx.tod.set(Some(86_400.0 + 12.0 * 3600.0 + 30.0));
    r.tick(&ctx);
    assert_eq!(outcomes(&r), vec![StepOutcome::Done]);
    assert_eq!(line(r.tick(&ctx)).as_deref(), Some("/time midnight"));
    // 23:50 is within the tolerance of midnight, across the day boundary.
    ctx.tod.set(Some(2.0 * 86_400.0 - 600.0));
    r.tick(&ctx);
    assert!(r.is_finished());
    assert_eq!(outcomes(&r), vec![StepOutcome::Done, StepOutcome::Done]);
}

#[test]
fn time_fails_when_the_server_ignores_it() {
    let ctx = Fake::new();
    ctx.tod.set(Some(3.0 * 3600.0));
    let mut r = StepRunner::new(&script(r#"Time("dusk")"#, 0));
    r.tick(&ctx);
    ctx.advance(TIME_TIMEOUT_S + 0.1);
    r.tick(&ctx);
    assert!(failed_with(&outcomes(&r)[0], "dusk"));
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
fn teleport_needs_horizontal_and_vertical_arrival() {
    let ctx = Fake::new();
    let mut r = StepRunner::new(&script("Teleport(wpos: (500.0, 600.0, 70.0))", 0));
    assert_eq!(
        line(r.tick(&ctx)).as_deref(),
        Some("/goto 500.00 600.00 70.00")
    );
    // Right column, wrong height: not there yet.
    ctx.at(500.0, 600.0, 20.0);
    assert_eq!(r.tick(&ctx), None);
    assert!(!r.is_finished());
    ctx.at(500.5, 600.0, 69.0);
    r.tick(&ctx);
    assert_eq!(outcomes(&r), vec![StepOutcome::Done]);
}

#[test]
fn teleport_already_there_sends_nothing() {
    let ctx = Fake::new();
    ctx.at(500.0, 600.0, 71.0);
    let mut r = StepRunner::new(&script("Teleport(wpos: (500.0, 600.0, 70.0))", 0));
    assert_eq!(r.tick(&ctx), None);
    assert_eq!(outcomes(&r), vec![StepOutcome::Done]);
}

#[test]
fn rejected_teleport_times_out_and_clears_the_target() {
    let ctx = Fake::new();
    let mut r = StepRunner::new(&script(
        "Teleport(wpos: (500.0, 600.0, 70.0)), WaitSeconds(1.0)",
        0,
    ));
    r.tick(&ctx);
    ctx.advance(TELEPORT_TIMEOUT_S + 0.1);
    r.tick(&ctx);
    assert!(failed_with(&outcomes(&r)[0], "refused"));
    // No drift re-teleport towards the failed target afterwards.
    assert!(frames(&mut r, &ctx, 5, 0.3).is_empty());
    assert_eq!(outcomes(&r)[1], StepOutcome::Done);
}

#[test]
fn rejected_cromatolis_teleport_fails() {
    let ctx = Fake::new();
    let mut r = StepRunner::new(&script(
        "CromatolisTeleport(px: 1024, py: 768, z_above_ground: 2.0)",
        0,
    ));
    assert_eq!(
        line(r.tick(&ctx)).as_deref(),
        Some("/cromatolis_goto 1024 768")
    );
    // The player never moves.
    let sent = frames(&mut r, &ctx, 40, CROMATOLIS_MOVE_TIMEOUT_S / 30.0);
    assert!(sent.is_empty());
    assert!(failed_with(&outcomes(&r)[0], "not moved"));
}

#[test]
fn cromatolis_pixel_beyond_i32_is_refused_before_sending() {
    let ctx = Fake::new();
    let mut r = StepRunner::new(&script(
        "CromatolisTeleport(px: 3000000000, py: 1, z_above_ground: 2.0)",
        0,
    ));
    assert_eq!(r.tick(&ctx), None);
    assert!(failed_with(&outcomes(&r)[0], "out of range"));
}

/// Teleport, fall to where the player comes to rest, then `/goto` to the
/// requested height above that.
#[test]
fn cromatolis_teleport_lands_then_corrects_height() {
    let ctx = Fake::new();
    let mut r = StepRunner::new(&script(
        "CromatolisTeleport(px: 1024, py: 768, z_above_ground: 10.0)",
        0,
    ));
    r.tick(&ctx);
    ctx.at(16384.0, 16384.0, 205.0);
    assert_eq!(r.tick(&ctx), None);
    // Falls onto a rock 3 blocks above the smooth surface, then rests.
    ctx.at(16384.0, 16384.0, 203.0);
    let sent = frames(&mut r, &ctx, REST_FRAMES + 1, 0.016);
    assert_eq!(sent, vec!["/goto 16384.00 16384.00 213.00"]);
    ctx.at(16384.0, 16384.0, 212.5);
    r.tick(&ctx);
    assert_eq!(outcomes(&r), vec![StepOutcome::Done]);
}

/// Already standing at that column: the server lifts the player straight up,
/// which is still a jump.
#[test]
fn cromatolis_teleport_same_column_vertical_move() {
    let ctx = Fake::new();
    ctx.at(10.0, 10.0, 100.0);
    let mut r = StepRunner::new(&script(
        "CromatolisTeleport(px: 1, py: 2, z_above_ground: 2.0)",
        0,
    ));
    r.tick(&ctx);
    ctx.at(10.0, 10.0, 105.0);
    r.tick(&ctx);
    ctx.at(10.0, 10.0, 100.0);
    // Resting at 100; the target 102 is within the arrival tolerance.
    let sent = frames(&mut r, &ctx, REST_FRAMES + 2, 0.016);
    assert!(sent.is_empty(), "{sent:?}");
    assert_eq!(outcomes(&r), vec![StepOutcome::Done]);
}

/// Small continuous movement (knockback, walking) is not mistaken for the
/// teleport.
#[test]
fn cromatolis_teleport_ignores_knockback() {
    let ctx = Fake::new();
    let mut r = StepRunner::new(&script(
        "CromatolisTeleport(px: 1, py: 2, z_above_ground: 30.0)",
        0,
    ));
    r.tick(&ctx);
    for i in 1..=10 {
        ctx.at(0.5 * i as f32, 0.0, 100.0);
        assert_eq!(r.tick(&ctx), None);
    }
    // Knockback ended and the player rests: still waiting for the jump.
    let sent = frames(&mut r, &ctx, REST_FRAMES + 2, 0.016);
    assert!(sent.is_empty());
    assert!(r.records().is_empty());
    ctx.at(900.0, 900.0, 300.0);
    r.tick(&ctx);
    let sent = frames(&mut r, &ctx, REST_FRAMES + 1, 0.016);
    assert_eq!(sent, vec!["/goto 900.00 900.00 330.00"]);
}

#[test]
fn wait_for_terrain_needs_loaded_meshed_and_settle_frames() {
    let ctx = Fake::new();
    let mut r = StepRunner::new(&script("WaitForTerrain(radius_chunks: 8)", 3));
    frames(&mut r, &ctx, 5, 0.3);
    assert_eq!(ctx.radius_asked.get(), Some(8));
    assert!(!r.is_finished());
    ctx.terrain.set(READY);
    frames(&mut r, &ctx, 3, 0.3);
    assert!(!r.is_finished());
    // Readiness lost (a new chunk arrived) restarts the settle count.
    ctx.terrain.set(TerrainStatus {
        total: 11,
        loaded: 10,
        meshed: 10,
    });
    frames(&mut r, &ctx, 1, 0.3);
    ctx.terrain.set(READY);
    frames(&mut r, &ctx, 3, 0.3);
    assert!(!r.is_finished());
    frames(&mut r, &ctx, 1, 0.3);
    assert_eq!(outcomes(&r), vec![StepOutcome::Done]);
}

#[test]
fn wait_for_terrain_is_throttled_and_confirms_at_the_end() {
    let ctx = Fake::new();
    let mut r = StepRunner::new(&script("WaitForTerrain()", 20));
    frames(&mut r, &ctx, 30, 0.016);
    // 30 frames over 0.48 s: one check at the start, one after 0.25 s.
    assert_eq!(ctx.ready_checks.get(), 2);
    ctx.terrain.set(READY);
    frames(&mut r, &ctx, 40, 0.016);
    assert!(r.is_finished());
    // A chunk lost between throttled checks is caught by the final check.
    let ctx = Fake::new();
    let mut r = StepRunner::new(&script("WaitForTerrain()", 2));
    ctx.terrain.set(READY);
    frames(&mut r, &ctx, 2, 0.016);
    ctx.terrain.set(NOT_READY);
    frames(&mut r, &ctx, 1, 0.016);
    assert!(!r.is_finished());
}

#[test]
fn empty_terrain_is_never_ready() {
    assert!(!TerrainStatus::default().ready());
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
fn wait_for_terrain_radius_must_fit_the_view_distance() {
    let ctx = Fake::new();
    ctx.view_distance.set(Some(6));
    let mut r = StepRunner::new(&script("WaitForTerrain()", 0));
    r.tick(&ctx);
    assert!(failed_with(&outcomes(&r)[0], "view distance of 6"));
    assert_eq!(ctx.ready_checks.get(), 0);
}

#[test]
fn wait_for_terrain_radius_is_clamped() {
    let ctx = Fake::new();
    ctx.view_distance.set(None);
    let mut r = StepRunner::new(&script("WaitForTerrain(radius_chunks: 100000)", 0));
    r.tick(&ctx);
    assert_eq!(ctx.radius_asked.get(), Some(MAX_RADIUS_CHUNKS));
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
fn dead_player_fails_waiting_steps_fast() {
    let ctx = Fake::new();
    ctx.dead.set(true);
    let mut r = StepRunner::new(&script(
        "Teleport(wpos: (100.0, 100.0, 50.0)), WaitForTerrain(), WaitSeconds(5.0)",
        0,
    ));
    let sent = frames(&mut r, &ctx, 3, 0.016);
    assert!(sent.is_empty());
    assert!(r.is_finished());
    assert!(outcomes(&r).iter().all(|o| failed_with(o, "dead")));
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
    ctx.advance(0.3);
    assert_eq!(r.tick(&ctx), None);
    assert_eq!(r.current_index(), 1);
    // Back at the target: terrain wait completes.
    ctx.at(100.0, 100.0, 50.0);
    frames(&mut r, &ctx, 2, 0.3);
    assert_eq!(r.current_index(), 2);
    // Drifting again: no second re-teleport, the step fails.
    ctx.at(110.0, 100.0, 50.0);
    assert_eq!(r.tick(&ctx), None);
    assert!(r.is_finished());
    let o = outcomes(&r);
    assert_eq!(o[0], StepOutcome::Done);
    assert_eq!(o[1], StepOutcome::Done);
    assert!(failed_with(&o[2], "drifted"));
}

/// Falling to the ground after a teleport is fine; sinking (or being thrown
/// up) after coming to rest is drift.
#[test]
fn vertical_drift_counts_after_rest() {
    let ctx = Fake::new();
    let mut r = StepRunner::new(&script(
        "Teleport(wpos: (100.0, 100.0, 60.0)), WaitSeconds(100.0)",
        0,
    ));
    r.tick(&ctx);
    ctx.at(100.0, 100.0, 60.0);
    r.tick(&ctx);
    ctx.at(100.0, 100.0, 50.0);
    assert!(frames(&mut r, &ctx, REST_FRAMES + 2, 0.016).is_empty());
    ctx.at(100.0, 100.0, 40.0);
    assert_eq!(
        line(r.tick(&ctx)).as_deref(),
        Some("/goto 100.00 100.00 60.00")
    );
}

/// A swimmer rises or sinks without ever resting: that is not drift, and
/// must not trigger a re-teleport or fail the wait.
#[test]
fn swimming_player_is_not_drift() {
    let ctx = Fake::new();
    ctx.in_liquid.set(true);
    let mut r = StepRunner::new(&script(
        "Teleport(wpos: (100.0, 100.0, -60.0)), WaitSeconds(3.0)",
        0,
    ));
    r.tick(&ctx);
    ctx.at(100.0, 100.0, -60.0);
    r.tick(&ctx);
    for i in 1..=(REST_FRAMES + 20) {
        ctx.at(100.0, 100.0, -60.0 + 0.2 * i as f32);
        assert!(
            line(r.tick(&ctx)).is_none(),
            "no re-teleport while swimming"
        );
    }
    ctx.advance(4.0);
    r.tick(&ctx);
    assert!(r.is_finished());
    assert!(matches!(outcomes(&r)[1], StepOutcome::Done));
}

/// Swimming only excuses vertical motion: a real horizontal displacement is
/// still reported (re-teleported once), while pure bobbing never is.
#[test]
fn swimming_horizontal_drift_is_reported() {
    let ctx = Fake::new();
    ctx.in_liquid.set(true);
    let mut r = StepRunner::new(&script(
        "Teleport(wpos: (100.0, 100.0, -60.0)), WaitSeconds(100.0)",
        0,
    ));
    r.tick(&ctx);
    ctx.at(100.0, 100.0, -60.0);
    r.tick(&ctx);
    // Bobbing up and down far beyond the tolerance: fine.
    ctx.at(100.0, 100.0, -60.0 + DRIFT_BLOCKS * 3.0);
    assert!(line(r.tick(&ctx)).is_none());
    // Pushed sideways while swimming: re-teleported.
    ctx.at(100.0 + DRIFT_BLOCKS + 1.0, 100.0, -60.0);
    assert_eq!(
        line(r.tick(&ctx)).as_deref(),
        Some("/goto 100.00 100.00 -60.00")
    );
}

/// A viewpoint far from the player can never have its terrain loaded: say so
/// at once instead of waiting out the terrain timeout.
#[test]
fn viewpoint_beyond_the_view_distance_fails_fast() {
    let ctx = Fake::new();
    ctx.at(0.0, 0.0, 100.0);
    let mut r = StepRunner::new(&script(
        "Viewpoint(cam: (0.0, 600.0, 120.0), look_at: (0.0, 0.0, 100.0))",
        0,
    ));
    for _ in 0..6 {
        r.tick(&ctx);
    }
    assert!(failed_with(&outcomes(&r)[0], "chunks from the player"));
    assert!(ctx.now.get() < 1.0, "no timeout was waited out");
}

#[test]
fn wait_seconds_restarts_after_a_reteleport() {
    let ctx = Fake::new();
    let mut r = StepRunner::new(&script(
        "Teleport(wpos: (0.0, 0.0, 100.0)), WaitSeconds(5.0)",
        0,
    ));
    r.tick(&ctx);
    r.tick(&ctx);
    ctx.advance(4.0);
    ctx.at(50.0, 0.0, 100.0);
    assert!(line(r.tick(&ctx)).is_some());
    ctx.advance(1.0);
    ctx.at(0.0, 0.0, 100.0);
    r.tick(&ctx);
    ctx.advance(4.0);
    r.tick(&ctx);
    assert!(!r.is_finished(), "timer restarted after the re-teleport");
    ctx.advance(1.5);
    r.tick(&ctx);
    assert!(r.is_finished());
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
    assert!(failed_with(&outcomes(&r)[1], "drifted"));
}

#[test]
fn steps_run_strictly_in_order_one_at_a_time() {
    let ctx = Fake::new();
    ctx.at(5.0, 5.0, 100.0);
    let mut r = StepRunner::new(&script(
        r#"Time("dawn"), WaitSeconds(1.0), Teleport(wpos: (0.0, 0.0, 100.0)), Time("dusk")"#,
        0,
    ));
    let mut sent = Vec::new();
    for i in 0..20 {
        if let Some(l) = line(r.tick(&ctx)) {
            sent.push(l);
        }
        if i == 6 {
            ctx.at(0.0, 0.0, 100.0);
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
fn exit_code_reflects_failed_steps() {
    let ctx = Fake::new();
    let mut ok = StepRunner::new(&script("WaitSeconds(0.1)", 0));
    ok.tick(&ctx);
    ctx.advance(1.0);
    ok.tick(&ctx);
    assert!(ok.is_finished());
    assert_eq!(finished_exit_code(&ok), None);

    // A time that never shows up: a soft failure, the run goes on.
    ctx.tod.set(Some(3.0 * 3600.0));
    let mut bad = StepRunner::new(&script(r#"Time("dusk"), WaitSeconds(0.1)"#, 0));
    frames(&mut bad, &ctx, 40, 1.0);
    assert!(bad.is_finished());
    assert!(!bad.screenshot_failed());
    assert_eq!(finished_exit_code(&bad), Some(STEP_FAILED_EXIT_CODE));
}

#[test]
fn exit_code_table() {
    assert_eq!(STEP_FAILED_EXIT_CODE, 1);
    assert_eq!(SETUP_EXIT_CODE, 2);
    assert_eq!(SCREENSHOT_FAILED_EXIT_CODE, 3);
}

#[test]
fn unwritable_screenshot_gives_exit_3_over_step_failure() {
    let ctx = Fake::new();
    ctx.terrain.set(READY);
    let mut r = StepRunner::new(&script(
        r#"Screenshot(name: "a"), Time("dusk"), WaitSeconds(0.1)"#,
        0,
    ));
    ctx.tod.set(Some(3.0 * 3600.0));
    // Request the shot.
    let mut sent = Vec::new();
    for _ in 0..40 {
        sent.extend(line(r.tick(&ctx)));
        if sent.iter().any(|s| s.contains("Screenshot")) {
            break;
        }
        ctx.advance(0.3);
    }
    assert!(sent.iter().any(|s| s.contains("Screenshot")), "{sent:?}");
    *ctx.shot.borrow_mut() = Some(ShotOutcome::Failed("disk full".into()));
    r.tick(&ctx);
    *ctx.shot.borrow_mut() = None;
    frames(&mut r, &ctx, 60, 1.0);
    assert!(r.is_finished());
    assert!(r.screenshot_failed());
    assert_eq!(finished_exit_code(&r), Some(SCREENSHOT_FAILED_EXIT_CODE));
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
    let corner: Vec<_> = chunks_in_radius(Vec2::new(0, 0), 1, Vec2::new(64, 64)).collect();
    assert_eq!(corner, vec![
        Vec2::new(0, 0),
        Vec2::new(1, 0),
        Vec2::new(0, 1)
    ]);
}

/// Every whitelisted time has the hour the server's `/time` sets for it.
#[test]
fn time_hours_match_server() {
    let names: Vec<_> = TIME_HOURS.iter().map(|(n, _)| *n).collect();
    assert_eq!(names, TIME_NAMES);
    let server = include_str!("../../../server/src/cmd.rs");
    for (name, hour) in TIME_HOURS {
        let arm = format!("Some(\"{name}\") => next_cycle(");
        let at = server
            .find(&arm)
            .unwrap_or_else(|| panic!("no /time arm for {name}"));
        let rest = &server[at..];
        let h = rest
            .split("from_hms_opt(")
            .nth(1)
            .and_then(|s| s.split(',').next())
            .and_then(|s| s.trim().parse::<u32>().ok())
            .unwrap_or_else(|| panic!("no hour for {name}"));
        assert_eq!(h, *hour, "{name}");
    }
}

/// The commands exist, need admin rights, and take the arguments used here.
#[test]
fn commands_match_the_command_table() {
    use common::cmd::ServerChatCommand;
    for (keyword, args) in [("goto", 3), ("cromatolis_goto", 2), ("time", 1)] {
        let cmd = ServerChatCommand::iter()
            .find(|c| c.keyword() == keyword)
            .unwrap_or_else(|| panic!("no /{keyword}"));
        assert!(cmd.needs_role().is_some(), "/{keyword} must be admin-only");
        let data = cmd.data();
        assert!(data.args.len() >= args, "/{keyword} argument count");
    }
}

// ---- camera, viewpoint, minimap, HUD and screenshot steps -----------------

#[test]
fn screenshot_file_names_are_plain() {
    assert_eq!(screenshot_file_name(1, "deck_noon"), "001_deck_noon.png");
    assert_eq!(screenshot_file_name(12, "a.b-c"), "012_a.b-c.png");
    for evil in [
        "../../etc/passwd",
        "a/b",
        "a\\b",
        ".hidden",
        "x\0y",
        "C:\\x",
        "a..b",
    ] {
        let n = screenshot_file_name(3, evil);
        assert!(
            !n.contains('/') && !n.contains('\\') && !n.contains(".."),
            "{n}"
        );
        assert!(!n.starts_with('.') && n.ends_with(".png"), "{n}");
        assert!(
            n.chars()
                .all(|c| c.is_ascii_alphanumeric() || "_-.".contains(c)),
            "{n}"
        );
    }
    assert_eq!(screenshot_file_name(1, ".x"), "001__x.png");
}

#[test]
fn camera_conventions() {
    let (yaw, pitch) = camera_orientation(90.0, -20.0);
    assert!((yaw - std::f32::consts::FRAC_PI_2).abs() < 1e-6);
    // Looking down is a positive engine pitch.
    assert!((pitch - 20f32.to_radians()).abs() < 1e-6);

    // Straight north (+Y) and level.
    let (y, p) = look_at_orientation(Vec3::zero(), Vec3::new(0.0, 10.0, 0.0)).unwrap();
    assert!(y.abs() < 1e-6 && p.abs() < 1e-6);
    // East (+X), 45 degrees down.
    let (y, p) = look_at_orientation(Vec3::new(0.0, 0.0, 10.0), Vec3::new(10.0, 0.0, 0.0)).unwrap();
    assert!((y - std::f32::consts::FRAC_PI_2).abs() < 1e-5);
    assert!((p - std::f32::consts::FRAC_PI_4).abs() < 1e-5);
    // Straight down stays finite.
    let (_, p) = look_at_orientation(Vec3::new(0.0, 0.0, 5.0), Vec3::zero()).unwrap();
    assert!((p - std::f32::consts::FRAC_PI_2).abs() < 1e-5);
    assert!(look_at_orientation(Vec3::one(), Vec3::one()).is_none());
}

#[test]
fn camera_step_applies_then_settles() {
    let ctx = Fake::new();
    ctx.camera_settled.set(false);
    let mut r = StepRunner::new(&script(
        "Camera(yaw_deg: 45.0, pitch_deg: -20.0, distance: 10.0), WaitSeconds(0.1)",
        0,
    ));
    let a = r.tick(&ctx);
    match a {
        Some(Action::Camera {
            yaw_rad,
            pitch_rad,
            distance,
        }) => {
            assert!((yaw_rad - 45f32.to_radians()).abs() < 1e-6);
            assert!((pitch_rad - 20f32.to_radians()).abs() < 1e-6);
            assert_eq!(distance, 10.0);
        },
        other => panic!("{other:?}"),
    }
    // Zoom not settled: waits, up to the cap, then moves on without failing.
    for _ in 0..5 {
        ctx.advance(0.1);
        assert_eq!(r.tick(&ctx), None);
    }
    assert!(r.records().is_empty());
    ctx.advance(super::driver::CAMERA_SETTLE_MAX_S);
    r.tick(&ctx);
    assert_eq!(outcomes(&r), vec![StepOutcome::Done]);
}

#[test]
fn camera_step_continues_once_settled() {
    let ctx = Fake::new();
    let mut r = StepRunner::new(&script(
        "Camera(yaw_deg: 0.0, pitch_deg: 0.0, distance: 5.0)",
        0,
    ));
    assert!(r.tick(&ctx).is_some());
    r.tick(&ctx);
    r.tick(&ctx);
    assert_eq!(outcomes(&r), vec![StepOutcome::Done]);
}

#[test]
fn viewpoint_waits_for_terrain_around_the_camera() {
    let ctx = Fake::new();
    ctx.terrain.set(NOT_READY);
    let mut r = StepRunner::new(&script(
        "Viewpoint(cam: (100.0, 200.0, 300.0), look_at: (100.0, 300.0, 300.0))",
        0,
    ));
    match r.tick(&ctx) {
        Some(Action::Viewpoint {
            cam,
            yaw_rad,
            pitch_rad,
        }) => {
            assert_eq!(cam, Vec3::new(100.0, 200.0, 300.0));
            assert!(yaw_rad.abs() < 1e-6 && pitch_rad.abs() < 1e-6);
        },
        other => panic!("{other:?}"),
    }
    frames(&mut r, &ctx, 4, 0.3);
    assert!(r.records().is_empty(), "terrain is not ready");
    assert_eq!(
        ctx.center_asked.get(),
        Some(Some(Vec2::new(100.0, 200.0))),
        "readiness is checked around the camera, not the player"
    );
    ctx.terrain.set(READY);
    frames(&mut r, &ctx, 5, 0.3);
    assert_eq!(outcomes(&r), vec![StepOutcome::Done]);
}

#[test]
fn viewpoint_terrain_timeout_is_a_soft_failure() {
    let ctx = Fake::new();
    ctx.terrain.set(NOT_READY);
    let mut r = StepRunner::new(&script(
        "Viewpoint(cam: (1.0, 2.0, 3.0), look_at: (1.0, 3.0, 3.0)), WaitSeconds(0.1)",
        0,
    ));
    r.tick(&ctx);
    r.tick(&ctx);
    ctx.advance(61.0);
    r.tick(&ctx);
    assert!(failed_with(&outcomes(&r)[0], "terrain not ready"));
}

#[test]
fn minimap_and_sample_steps() {
    let ctx = Fake::new();
    let mut r = StepRunner::new(&script(
        r#"Minimap(show: true), Sample(label: "on"), Minimap(show: false)"#,
        0,
    ));
    assert_eq!(r.tick(&ctx), Some(Action::Minimap(true)));
    assert_eq!(r.tick(&ctx), None);
    assert_eq!(r.tick(&ctx), Some(Action::Minimap(false)));
    assert!(r.is_finished());
    assert_eq!(finished_exit_code(&r), None);
}

/// Tick until an action that is not `None` appears.
fn next_action(r: &mut StepRunner, ctx: &Fake, max: u32, dt: f64) -> Option<Action> {
    for _ in 0..max {
        if let Some(a) = r.tick(ctx) {
            return Some(a);
        }
        ctx.advance(dt);
    }
    None
}

#[test]
fn screenshot_hides_hud_waits_and_restores() {
    let ctx = Fake::new();
    ctx.terrain.set(READY);
    let mut r = StepRunner::new(&script(r#"Screenshot(name: "deck")"#, 2));
    assert_eq!(r.tick(&ctx), Some(Action::Hud(false)));
    // The capture is not requested before the HUD had its frames and the
    // terrain settled.
    let mut shots = 0;
    let mut saw = None;
    for _ in 0..(HUD_HIDE_FRAMES + 40) {
        ctx.advance(0.3);
        if let Some(a) = r.tick(&ctx) {
            saw = Some(a);
            shots += 1;
            break;
        }
    }
    assert_eq!(shots, 1);
    assert_eq!(
        saw,
        Some(Action::Screenshot {
            file_name: "001_deck.png".into()
        })
    );
    assert!(r.records().is_empty(), "waits for the file");
    *ctx.shot.borrow_mut() = Some(ShotOutcome::Written);
    // Done, and the HUD comes back in the same tick.
    assert_eq!(r.tick(&ctx), Some(Action::Hud(true)));
    assert_eq!(outcomes(&r), vec![StepOutcome::Done]);
    assert_eq!(finished_exit_code(&r), None);
}

#[test]
fn screenshot_without_hide_hud_sends_no_hud_actions() {
    let ctx = Fake::new();
    ctx.terrain.set(READY);
    let src = "ShotScript(version: 1, window: (1920, 1080), defaults: (view_distance: 12, \
               terrain_radius_chunks: 6, terrain_timeout_s: 60.0, settle_frames: 0, hide_hud: \
               false), steps: [Screenshot(name: \"a\")])";
    let s = ShotScript::parse(src, "t.ron").unwrap();
    let mut r = StepRunner::new(&s);
    let a = next_action(&mut r, &ctx, 40, 0.3);
    assert_eq!(
        a,
        Some(Action::Screenshot {
            file_name: "001_a.png".into()
        })
    );
    *ctx.shot.borrow_mut() = Some(ShotOutcome::Written);
    assert_eq!(r.tick(&ctx), None);
    assert!(r.is_finished());
}

#[test]
fn screenshot_still_taken_when_terrain_times_out() {
    let ctx = Fake::new();
    ctx.terrain.set(NOT_READY);
    let mut r = StepRunner::new(&script(r#"Screenshot(name: "late")"#, 0));
    r.tick(&ctx); // hide hud
    frames(&mut r, &ctx, HUD_HIDE_FRAMES, 0.1);
    ctx.advance(61.0);
    let a = next_action(&mut r, &ctx, 3, 0.1);
    assert!(matches!(a, Some(Action::Screenshot { .. })), "{a:?}");
    *ctx.shot.borrow_mut() = Some(ShotOutcome::Written);
    r.tick(&ctx);
    assert!(failed_with(&outcomes(&r)[0], "terrain not ready"));
    // A terrain problem is a step failure, not a write failure.
    assert_eq!(finished_exit_code(&r), Some(STEP_FAILED_EXIT_CODE));
}

#[test]
fn dropped_screenshot_is_retried_then_fails_with_exit_3() {
    let ctx = Fake::new();
    ctx.terrain.set(READY);
    let src = "ShotScript(version: 1, window: (1920, 1080), defaults: (view_distance: 12, \
               terrain_radius_chunks: 6, terrain_timeout_s: 60.0, settle_frames: 0, hide_hud: \
               false), steps: [Screenshot(name: \"a\")])";
    let mut r = StepRunner::new(&ShotScript::parse(src, "t.ron").unwrap());
    assert!(next_action(&mut r, &ctx, 40, 0.3).is_some());
    *ctx.shot.borrow_mut() = Some(ShotOutcome::Dropped);
    for _ in 0..SCREENSHOT_RETRIES {
        assert!(matches!(r.tick(&ctx), Some(Action::Screenshot { .. })));
    }
    assert_eq!(r.tick(&ctx), None);
    assert!(failed_with(&outcomes(&r)[0], "discarded"));
    assert_eq!(finished_exit_code(&r), Some(SCREENSHOT_FAILED_EXIT_CODE));
}

#[test]
fn screenshot_that_never_arrives_times_out_with_exit_3() {
    let ctx = Fake::new();
    ctx.terrain.set(READY);
    let src = "ShotScript(version: 1, window: (1920, 1080), defaults: (view_distance: 12, \
               terrain_radius_chunks: 6, terrain_timeout_s: 60.0, settle_frames: 0, hide_hud: \
               false), steps: [Screenshot(name: \"a\")])";
    let mut r = StepRunner::new(&ShotScript::parse(src, "t.ron").unwrap());
    assert!(next_action(&mut r, &ctx, 40, 0.3).is_some());
    ctx.advance(SCREENSHOT_TIMEOUT_S + 1.0);
    r.tick(&ctx);
    assert!(failed_with(&outcomes(&r)[0], "not written"));
    assert_eq!(finished_exit_code(&r), Some(SCREENSHOT_FAILED_EXIT_CODE));
}

#[test]
fn screenshot_after_viewpoint_checks_terrain_around_the_camera() {
    let ctx = Fake::new();
    // The abstract world here puts the lens far from the player; the
    // far-lens guard is covered by its own test.
    ctx.view_distance.set(None);
    ctx.terrain.set(READY);
    let mut r = StepRunner::new(&script(
        r#"Viewpoint(cam: (500.0, 600.0, 90.0), look_at: (500.0, 700.0, 80.0)),
           Screenshot(name: "far")"#,
        0,
    ));
    let a = next_action(&mut r, &ctx, 60, 0.3);
    assert!(matches!(a, Some(Action::Viewpoint { .. })), "{a:?}");
    let mut seen = None;
    for _ in 0..60 {
        ctx.advance(0.3);
        if let Some(a) = r.tick(&ctx) {
            seen = Some(a);
            if matches!(seen, Some(Action::Screenshot { .. })) {
                break;
            }
        }
    }
    assert!(matches!(seen, Some(Action::Screenshot { .. })), "{seen:?}");
    assert_eq!(ctx.center_asked.get(), Some(Some(Vec2::new(500.0, 600.0))));
}

#[test]
fn ordinals_sort_past_ninety_nine() {
    assert_eq!(screenshot_file_name(100, "x"), "100_x.png");
    assert!(screenshot_file_name(99, "x") < screenshot_file_name(100, "x"));
}

#[test]
fn teleport_after_viewpoint_keeps_watching_the_camera() {
    let ctx = Fake::new();
    // The abstract world here puts the lens far from the player; the
    // far-lens guard is covered by its own test.
    ctx.view_distance.set(None);
    ctx.terrain.set(READY);
    ctx.at(10.0, 10.0, 100.0);
    let mut r = StepRunner::new(&script(
        r#"Viewpoint(cam: (500.0, 600.0, 90.0), look_at: (500.0, 700.0, 80.0)),
           Teleport(wpos: (10.0, 10.0, 100.0)),
           Screenshot(name: "after")"#,
        0,
    ));
    let mut saw_shot = false;
    for _ in 0..80 {
        ctx.advance(0.3);
        if matches!(r.tick(&ctx), Some(Action::Screenshot { .. })) {
            saw_shot = true;
            break;
        }
    }
    assert!(saw_shot);
    assert_eq!(
        ctx.center_asked.get(),
        Some(Some(Vec2::new(500.0, 600.0))),
        "the lens is still at the viewpoint"
    );
}

#[test]
fn camera_step_after_viewpoint_returns_to_the_player() {
    let ctx = Fake::new();
    ctx.terrain.set(READY);
    let mut r = StepRunner::new(&script(
        r#"Viewpoint(cam: (500.0, 600.0, 90.0), look_at: (500.0, 700.0, 80.0)),
           Camera(yaw_deg: 0.0, pitch_deg: -10.0, distance: 8.0),
           Screenshot(name: "back")"#,
        0,
    ));
    for _ in 0..80 {
        ctx.advance(0.3);
        if matches!(r.tick(&ctx), Some(Action::Screenshot { .. })) {
            break;
        }
    }
    assert_eq!(ctx.center_asked.get(), Some(None), "player-centred again");
}

// ---- report.json ----

mod report_tests {
    use super::*;
    use crate::shot_script::report::{
        self, FileFacts, REPORT_FILE, Report, RunInfo, StepStatus, build_report, file_facts,
        write_report,
    };
    use std::path::{Path, PathBuf};

    fn info(abort_exit_code: i32) -> RunInfo {
        RunInfo {
            script_name: Some("s.ron".into()),
            script_sha256: Some("ab".repeat(32)),
            server: "127.0.0.1:14004".into(),
            started_at: "2026-10-15T10:00:00.000Z".into(),
            ended_at: "2026-10-15T10:01:00.000Z".into(),
            duration_s: 60.0,
            tool_version: None,
            engine_commit: Some("0123abcd".into()),
            client_view_distance: Some(12),
            abort_reason: None,
            abort_exit_code,
        }
    }

    fn no_files(_: &str) -> FileFacts { None }

    fn scratch(tag: &str) -> PathBuf {
        use std::os::unix::fs::DirBuilderExt;
        let dir = std::env::temp_dir().join(format!(
            "shot-report-{tag}-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::DirBuilder::new().mode(0o700).create(&dir).unwrap();
        dir
    }

    /// Run `steps` with time and the shot outcome driven by the test until
    /// the runner finishes.
    fn run_mixed(shot: ShotOutcome) -> StepRunner {
        let ctx = Fake::new();
        ctx.terrain.set(READY);
        ctx.tod.set(Some(3.0 * 3600.0));
        let mut r = StepRunner::new(&script(
            r#"Teleport(wpos: (10.0, 20.0, 100.0)), Time("dusk"), Screenshot(name: "a"), Sample(label: "end")"#,
            0,
        ));
        for _ in 0..400 {
            if let Some(a) = r.tick(&ctx) {
                match a {
                    Action::SendCommand(c) if c.to_string().starts_with("/goto") => {
                        ctx.at(10.0, 20.0, 100.0 + 0.001);
                        ctx.advance(0.1);
                        ctx.at(10.0, 20.0, 100.0);
                    },
                    Action::Screenshot { .. } => *ctx.shot.borrow_mut() = Some(shot.clone()),
                    _ => {},
                }
            }
            ctx.advance(0.5);
            if r.is_finished() {
                break;
            }
        }
        assert!(r.is_finished());
        r
    }

    #[test]
    fn mixed_run_reports_each_step() {
        let dir = scratch("mixed");
        let r = run_mixed(ShotOutcome::Written);
        // Pretend the renderer wrote the file.
        let png = b"png bytes";
        std::fs::write(dir.join("001_a.png"), png).unwrap();
        let rep = build_report(&r, &info(1), &|f| file_facts(&dir, f));
        assert_eq!(rep.steps.len(), 4);
        assert_eq!(rep.steps_ok, 3);
        assert_eq!(rep.steps_failed, 1);
        assert_eq!(rep.steps_skipped, 0);
        assert_eq!(rep.steps_total, 4);
        assert_eq!(Some(rep.exit_code), finished_exit_code(&r));
        assert_eq!(rep.exit_code, STEP_FAILED_EXIT_CODE);
        let tp = &rep.steps[0];
        assert_eq!((tp.kind.as_str(), tp.status), ("Teleport", StepStatus::Ok));
        assert_eq!(tp.position_before, Some([0.0, 0.0, 100.0]));
        assert_eq!(tp.position_after, Some([10.0, 20.0, 100.0]));
        let time = &rep.steps[1];
        assert_eq!(time.status, StepStatus::Failed);
        assert_eq!(time.name.as_deref(), Some("dusk"));
        assert!(time.failure_reason.as_deref().unwrap().contains("dusk"));
        assert_eq!(time.position_before, None, "positions are for teleports");
        let shot = &rep.steps[2];
        assert_eq!(shot.status, StepStatus::Ok);
        let s = shot.screenshot.as_ref().unwrap();
        assert_eq!(s.file, "001_a.png");
        assert!(s.written);
        assert_eq!(s.bytes, Some(png.len() as u64));
        assert_eq!(s.sha256.as_deref(), Some(report::sha256_hex(png).as_str()));
        let t = shot.terrain.as_ref().unwrap();
        assert_eq!(
            (t.chunks_total, t.chunks_loaded, t.chunks_meshed),
            (10, 10, 10)
        );
        assert!(t.ready && !t.timed_out);
        assert!(shot.duration_s.unwrap() >= 0.0);
        assert_eq!(rep.view_distance.script, 12);
        assert_eq!(rep.view_distance.client, Some(12));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn screenshot_failure_reports_exit_3_and_valid_json() {
        let dir = scratch("exit3");
        let r = run_mixed(ShotOutcome::Failed("disk full".into()));
        let rep = build_report(&r, &info(1), &no_files);
        assert_eq!(rep.exit_code, SCREENSHOT_FAILED_EXIT_CODE);
        assert_eq!(Some(rep.exit_code), finished_exit_code(&r));
        let shot = &rep.steps[2];
        assert_eq!(shot.status, StepStatus::Failed);
        assert_eq!(shot.failure_reason.as_deref(), Some("disk full"));
        let s = shot.screenshot.as_ref().unwrap();
        assert!(!s.written && s.bytes.is_none() && s.sha256.is_none());
        let path = write_report(&dir, &rep).unwrap();
        assert_eq!(path, dir.join(REPORT_FILE));
        let back: Report =
            serde_json::from_slice(&std::fs::read(&path).unwrap()).expect("valid json");
        assert_eq!(back, rep);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn terrain_timeout_is_reported() {
        let ctx = Fake::new();
        ctx.terrain.set(NOT_READY);
        let mut r = StepRunner::new(&script("WaitForTerrain(timeout_s: 5.0)", 0));
        frames(&mut r, &ctx, 40, 0.5);
        assert!(r.is_finished());
        let rep = build_report(&r, &info(1), &no_files);
        let t = rep.steps[0].terrain.as_ref().unwrap();
        assert!(t.timed_out && !t.ready);
        assert_eq!(
            (t.chunks_total, t.chunks_loaded, t.chunks_meshed),
            (10, 10, 7)
        );
        assert_eq!(rep.exit_code, STEP_FAILED_EXIT_CODE);
    }

    #[test]
    fn unfinished_run_marks_the_rest_skipped_with_the_abort_code() {
        let ctx = Fake::new();
        let r = StepRunner::new(&script(r#"Time("noon"), WaitSeconds(1.0)"#, 0));
        let mut i = info(2);
        i.abort_reason = Some("login failed".into());
        let rep = build_report(&r, &i, &no_files);
        assert_eq!(rep.steps_skipped, 2);
        assert_eq!(rep.steps_ok + rep.steps_failed, 0);
        assert_eq!(rep.exit_code, 2);
        assert_eq!(rep.abort_reason.as_deref(), Some("login failed"));
        assert!(rep.steps.iter().all(|s| s.started_s.is_none()));
        drop(ctx);
    }

    #[test]
    fn running_step_is_aborted_not_skipped_and_codes_agree() {
        let ctx = Fake::new();
        ctx.terrain.set(READY);
        let mut r = StepRunner::new(&script(
            r#"Time("noon"), WaitSeconds(100.0), Sample(label: "z")"#,
            0,
        ));
        ctx.tod.set(Some(12.0 * 3600.0));
        for _ in 0..6 {
            r.tick(&ctx);
            ctx.advance(0.5);
        }
        assert!(!r.is_finished());
        let reason = crate::shot_script::driver::abort_reason(&r);
        assert!(
            reason.as_deref().unwrap().contains("step index 1"),
            "{reason:?}"
        );
        assert_eq!(
            crate::shot_script::driver::abort_code(&r),
            STEP_FAILED_EXIT_CODE
        );
        let mut i = info(crate::shot_script::driver::abort_code(&r));
        i.abort_reason = reason;
        let rep = build_report(&r, &i, &no_files);
        let st: Vec<_> = rep.steps.iter().map(|s| s.status).collect();
        assert_eq!(st, [
            StepStatus::Ok,
            StepStatus::Aborted,
            StepStatus::Skipped
        ]);
        assert_eq!((rep.steps_aborted, rep.steps_skipped), (1, 1));
        assert!(rep.steps[1].started_s.is_some() && rep.steps[1].ended_s.is_none());
        assert!(rep.steps[1].failure_reason.is_some());
        assert_eq!(rep.exit_code, 1);
    }

    #[test]
    fn abort_code_agrees_with_the_report_on_early_ends() {
        use crate::shot_script::driver::{abort_code, abort_reason};
        // (a) a screenshot already failed, then the session ends early.
        let mut r = run_mixed(ShotOutcome::Failed("disk full".into()));
        assert!(r.is_finished());
        assert_eq!(abort_code(&r), SCREENSHOT_FAILED_EXIT_CODE);
        assert_eq!(abort_reason(&r), None, "a finished script is never aborted");
        let rep = build_report(&r, &info(abort_code(&r)), &no_files);
        assert_eq!(rep.exit_code, abort_code(&r));
        // (b) finished in its last tick with every step ok: code 0, no abort.
        let ctx = Fake::new();
        r = StepRunner::new(&script("WaitSeconds(0.1)", 0));
        for _ in 0..10 {
            r.tick(&ctx);
            ctx.advance(0.5);
        }
        assert!(r.is_finished());
        assert_eq!((abort_code(&r), abort_reason(&r)), (0, None));
        assert_eq!(build_report(&r, &info(1), &no_files).exit_code, 0);
        // Unfinished with a failed screenshot recorded keeps code 3.
        let ctx = Fake::new();
        ctx.terrain.set(READY);
        let mut r = StepRunner::new(&script(r#"Screenshot(name: "a"), WaitSeconds(100.0)"#, 0));
        for _ in 0..40 {
            if matches!(r.tick(&ctx), Some(Action::Screenshot { .. })) {
                *ctx.shot.borrow_mut() = Some(ShotOutcome::Failed("x".into()));
            }
            ctx.advance(0.5);
            if r.screenshot_failed() {
                break;
            }
        }
        assert!(!r.is_finished() && r.screenshot_failed());
        assert_eq!(abort_code(&r), SCREENSHOT_FAILED_EXIT_CODE);
        assert!(abort_reason(&r).is_some());
    }

    #[test]
    fn engine_commit_maps_the_zero_hash_to_none() {
        assert_eq!(report::engine_commit(0), None);
        assert_eq!(report::engine_commit(0xabc).as_deref(), Some("00000abc"));
    }

    #[test]
    fn json_has_the_documented_fields_and_leaks_no_paths() {
        let dir = scratch("fields");
        let r = run_mixed(ShotOutcome::Written);
        let rep = build_report(&r, &info(1), &no_files);
        let path = write_report(&dir, &rep).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        let v: serde_json::Value = serde_json::from_str(&text).unwrap();
        for k in [
            "schema_version",
            "script",
            "server",
            "started_at",
            "ended_at",
            "duration_s",
            "tool_version",
            "engine_commit",
            "view_distance",
            "steps_total",
            "steps_ok",
            "steps_failed",
            "steps_skipped",
            "steps_aborted",
            "exit_code",
            "abort_reason",
            "steps",
        ] {
            assert!(v.get(k).is_some(), "missing {k}");
        }
        assert!(v["tool_version"].is_null());
        for k in [
            "index",
            "kind",
            "name",
            "status",
            "failure_reason",
            "started_s",
            "ended_s",
            "duration_s",
            "screenshot",
            "terrain",
            "position_before",
            "position_after",
        ] {
            assert!(v["steps"][0].get(k).is_some(), "missing step.{k}");
        }
        assert_eq!(v["steps"][1]["status"], "failed");
        let dir_str = dir.to_string_lossy();
        assert!(!text.contains(&*dir_str), "out dir leaked");
        assert!(!text.contains("/Users/") && !text.contains("/home/"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn write_is_atomic_private_and_replaces() {
        use std::os::unix::fs::PermissionsExt;
        let dir = scratch("atomic");
        let r = run_mixed(ShotOutcome::Written);
        let rep = build_report(&r, &info(1), &no_files);
        std::fs::write(dir.join(REPORT_FILE), b"old").unwrap();
        let path = write_report(&dir, &rep).unwrap();
        let mode = std::fs::metadata(&path).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600);
        let names: Vec<_> = std::fs::read_dir(&dir)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(names, vec![REPORT_FILE.to_string()], "no partial left");
        // A stale partial of the same name fails cleanly and keeps the old
        // report intact.
        let partial = dir.join(format!("{REPORT_FILE}.{}.partial", std::process::id()));
        std::fs::write(&partial, b"x").unwrap();
        let before = std::fs::read(&path).unwrap();
        assert!(write_report(&dir, &rep).is_err());
        assert_eq!(std::fs::read(&path).unwrap(), before);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn write_refuses_bad_directories_and_symlinked_targets() {
        let dir = scratch("refuse");
        let r = run_mixed(ShotOutcome::Written);
        let rep = build_report(&r, &info(1), &no_files);
        assert!(write_report(Path::new("relative/dir"), &rep).is_err());
        assert!(write_report(&dir.join("..").join("x"), &rep).is_err());
        // A symlink planted as report.json is replaced, never followed.
        let victim = dir.join("victim");
        std::fs::write(&victim, b"keep").unwrap();
        std::os::unix::fs::symlink(&victim, dir.join(REPORT_FILE)).unwrap();
        write_report(&dir, &rep).unwrap();
        assert_eq!(std::fs::read(&victim).unwrap(), b"keep");
        assert!(
            serde_json::from_slice::<Report>(&std::fs::read(dir.join(REPORT_FILE)).unwrap())
                .is_ok()
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn file_facts_only_reads_plain_files_in_the_directory() {
        let dir = scratch("facts");
        std::fs::write(dir.join("a.png"), b"abc").unwrap();
        assert_eq!(file_facts(&dir, "a.png").unwrap().0, 3);
        assert!(file_facts(&dir, "missing.png").is_none());
        assert!(file_facts(&dir, "../a.png").is_none());
        assert!(file_facts(&dir, "").is_none());
        std::os::unix::fs::symlink(dir.join("a.png"), dir.join("link.png")).unwrap();
        assert!(file_facts(&dir, "link.png").is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
