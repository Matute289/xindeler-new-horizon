use super::{
    ShotScript,
    driver::{
        Action, CROMATOLIS_MOVE_TIMEOUT_S, DRIFT_BLOCKS, MAX_RADIUS_CHUNKS, REST_FRAMES,
        RunnerContext, ShotCommand, StepOutcome, StepRunner, TELEPORT_TIMEOUT_S, TIME_HOURS,
        TIME_TIMEOUT_S, TerrainStatus, chunks_in_radius, finished_exit_code,
    },
    login::STEP_FAILED_EXIT_CODE,
    schema::TIME_NAMES,
};
use std::cell::Cell;
use vek::{Vec2, Vec3};

/// A fake world: the test moves the clock, the player and the terrain.
struct Fake {
    now: Cell<f64>,
    pos: Cell<Option<Vec3<f32>>>,
    dead: Cell<bool>,
    tod: Cell<Option<f64>>,
    view_distance: Cell<Option<u32>>,
    terrain: Cell<TerrainStatus>,
    radius_asked: Cell<Option<u32>>,
    ready_checks: Cell<u32>,
}

impl Fake {
    fn new() -> Self {
        Self {
            now: Cell::new(0.0),
            pos: Cell::new(Some(Vec3::new(0.0, 0.0, 100.0))),
            dead: Cell::new(false),
            tod: Cell::new(None),
            view_distance: Cell::new(Some(12)),
            terrain: Cell::new(NOT_READY),
            radius_asked: Cell::new(None),
            ready_checks: Cell::new(0),
        }
    }

    fn advance(&self, s: f64) { self.now.set(self.now.get() + s); }

    fn at(&self, x: f32, y: f32, z: f32) { self.pos.set(Some(Vec3::new(x, y, z))); }
}

impl RunnerContext for Fake {
    fn now_s(&self) -> f64 { self.now.get() }

    fn player_pos(&self) -> Option<Vec3<f32>> { self.pos.get() }

    fn player_dead(&self) -> bool { self.dead.get() }

    fn time_of_day(&self) -> Option<f64> { self.tod.get() }

    fn view_distance(&self) -> Option<u32> { self.view_distance.get() }

    fn terrain_ready(&self, radius_chunks: u32) -> bool {
        self.radius_asked.set(Some(radius_chunks));
        self.ready_checks.set(self.ready_checks.get() + 1);
        self.terrain.get().ready()
    }

    fn terrain_status(&self, _radius_chunks: u32) -> TerrainStatus { self.terrain.get() }
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

#[test]
fn exit_code_reflects_failed_steps() {
    let ctx = Fake::new();
    let mut ok = StepRunner::new(&script("WaitSeconds(0.1)", 0));
    ctx.advance(1.0);
    ok.tick(&ctx);
    assert!(ok.is_finished());
    assert_eq!(finished_exit_code(&ok), None);

    let mut bad = StepRunner::new(&script(r#"Screenshot(name: "a"), WaitSeconds(0.1)"#, 0));
    frames(&mut bad, &ctx, 3, 1.0);
    assert!(bad.is_finished());
    assert_eq!(finished_exit_code(&bad), Some(STEP_FAILED_EXIT_CODE));
    assert_ne!(STEP_FAILED_EXIT_CODE, 0);
    assert_ne!(STEP_FAILED_EXIT_CODE, super::login::SETUP_EXIT_CODE);
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
