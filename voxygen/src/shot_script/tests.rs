use super::*;

const EXAMPLE: &str = include_str!("../../shot-script/example.ron");

/// A minimal valid script with `steps` substituted in.
fn script(steps: &str) -> String {
    format!(
        "ShotScript(\n    version: 1,\n    window: (1920, 1080),\n    defaults: (view_distance: \
         12, terrain_radius_chunks: 6, terrain_timeout_s: 180.0, settle_frames: 30, hide_hud: \
         true),\n    steps: [\n{steps}\n    ],\n)\n"
    )
}

fn err(src: &str) -> ScriptError {
    ShotScript::parse(src, "test.ron").expect_err("script should be rejected")
}

#[test]
fn example_script_parses() {
    let s = ShotScript::parse(EXAMPLE, "example.ron").expect("example must be valid");
    assert_eq!(s.version, 1);
    assert_eq!(s.window, (1920, 1080));
    assert_eq!(s.steps.len(), 15);
    assert_eq!(s.steps[0], Step::Time("noon".into()));
    assert_eq!(s.steps[2], Step::WaitForTerrain {
        radius_chunks: None,
        timeout_s: None,
    });
    assert_eq!(s.steps[12], Step::WaitForTerrain {
        radius_chunks: Some(8),
        timeout_s: Some(240.0),
    });
    assert_eq!(s.steps[14], Step::Screenshot {
        name: "citadel_from_1500m".into()
    });
}

#[test]
fn every_step_kind_parses() {
    let s = ShotScript::parse(
        &script(
            "Time(\"dusk\"), Teleport(wpos: (1.0, 2.0, 3.0)), CromatolisTeleport(px: 1, py: 2, \
             z_above_ground: 2.0), WaitForTerrain(timeout_s: 10.0), Camera(yaw_deg: 0.0, \
             pitch_deg: -90.0, distance: 0.0), Viewpoint(cam: (0.0, 0.0, 10.0), look_at: (0.0, \
             0.0, 0.0)), Minimap(show: false), WaitSeconds(0.5), Sample(label: \"a\"), \
             Screenshot(name: \"b-1.x\")",
        ),
        "t.ron",
    )
    .unwrap();
    let kinds: Vec<_> = s.steps.iter().map(Step::kind).collect();
    assert_eq!(kinds, [
        "Time",
        "Teleport",
        "CromatolisTeleport",
        "WaitForTerrain",
        "Camera",
        "Viewpoint",
        "Minimap",
        "WaitSeconds",
        "Sample",
        "Screenshot"
    ]);
}

#[test]
fn unknown_step_is_rejected_with_line() {
    let e = err(&script(
        "        Time(\"noon\"),\n        RunCommand(\"/kill\"),",
    ));
    assert_eq!(e.file, "test.ron");
    assert_eq!(e.line, Some(7), "{e}");
    assert!(e.to_string().starts_with("test.ron:7:"), "{e}");
}

#[test]
fn unknown_field_is_rejected() {
    let e = err(&script("Teleport(wpos: (1.0, 2.0, 3.0), force: true)"));
    assert!(e.line.is_some());
    assert!(e.message.contains("force"), "{e}");
    let e = err(&script("Time(\"noon\")").replace("version: 1,", "version: 1, extra: 3,"));
    assert!(e.message.contains("extra"), "{e}");
}

#[test]
fn bad_numbers_are_rejected() {
    // Not a number at all: a parse error with a position.
    let e = err(&script("WaitSeconds(\"five\")"));
    assert!(e.line.is_some(), "{e}");
    // Negative into an unsigned field.
    let e = err(&script(
        "CromatolisTeleport(px: -1, py: 2, z_above_ground: 2.0)",
    ));
    assert!(e.line.is_some(), "{e}");
    // Parsed but out of range: validation names step and line.
    let e = err(&script(
        "        Time(\"noon\"),\n        WaitSeconds(0.0),",
    ));
    assert_eq!(e.line, Some(7), "{e}");
    assert!(e.message.starts_with("step 2 (WaitSeconds)"), "{e}");
    let e = err(&script("Teleport(wpos: (1.0, inf, 3.0))"));
    assert!(e.message.contains("wpos.y"), "{e}");
    let e = err(&script(
        "Camera(yaw_deg: 0.0, pitch_deg: 91.0, distance: 1.0)",
    ));
    assert!(e.message.contains("pitch_deg"), "{e}");
    let e = err(&script(
        "Camera(yaw_deg: 0.0, pitch_deg: 0.0, distance: -1.0)",
    ));
    assert!(e.message.contains("distance"), "{e}");
    let e = err(&script(
        "Camera(yaw_deg: 0.0, pitch_deg: 0.0, distance: 500.5)",
    ));
    assert!(e.message.contains("at most 500"), "{e}");
    assert!(
        ShotScript::parse(
            &script("Camera(yaw_deg: 0.0, pitch_deg: 0.0, distance: 500.0)"),
            "t.ron"
        )
        .is_ok()
    );
    let e = err(&script("WaitForTerrain(radius_chunks: 0)"));
    assert!(e.message.contains("radius_chunks"), "{e}");
    let e = err(&script("Time(\"noon\")").replace("(1920, 1080)", "(0, 1080)"));
    assert!(e.message.contains("window"), "{e}");
    let e =
        err(&script("Time(\"noon\")")
            .replace("terrain_timeout_s: 180.0", "terrain_timeout_s: -1.0"));
    assert!(e.message.contains("terrain_timeout_s"), "{e}");
}

#[test]
fn wrong_version_is_rejected() {
    let e = err(&script("Time(\"noon\")").replace("version: 1,", "version: 2,"));
    assert!(e.message.contains("version 2"), "{e}");
}

#[test]
fn unknown_time_is_rejected() {
    // Only named times: a free string could otherwise carry extra arguments.
    let e = err(&script("Time(\"noon extra\")"));
    assert!(e.message.contains("unknown time"), "{e}");
    let e = err(&script("Time(\"12\")"));
    assert!(e.message.contains("unknown time"), "{e}");
}

#[test]
fn empty_script_is_rejected() {
    let e = err(&script(""));
    assert_eq!(e.message, "script has no steps");
    assert_eq!(e.to_string(), "test.ron: script has no steps");
    // An empty file is a parse error, not a panic.
    let e = err("");
    assert!(e.line.is_some(), "{e}");
}

#[test]
fn duplicate_screenshot_names_are_rejected() {
    let e = err(&script(
        "        Screenshot(name: \"a\"),\n        Sample(label: \"a\"),\n        // \
         Screenshot(name: \"commented out\"),\n        Screenshot(name: \"a\"),",
    ));
    assert!(e.message.contains("duplicate screenshot name \"a\""), "{e}");
    assert!(e.message.starts_with("step 3 (Screenshot)"), "{e}");
    // The commented-out step is skipped when locating the line.
    assert_eq!(e.line, Some(9), "{e}");
}

#[test]
fn unsafe_screenshot_names_are_rejected() {
    for (name, reason) in [
        ("..", "\"..\""),
        ("../escape", "path separator"),
        ("a..b", "\"..\""),
        ("/etc/passwd", "path separator"),
        ("sub/dir", "path separator"),
        ("sub\\\\dir", "path separator"),
        ("nul\\0byte", "NUL"),
        ("C:evil", "plain name"),
        (".hidden", "start with '.'"),
        ("", "empty"),
        ("sp ace", "only ASCII"),
        ("ünï", "only ASCII"),
    ] {
        let e = err(&script(&format!("Screenshot(name: \"{name}\")")));
        assert!(
            e.message.contains(reason),
            "name {name:?}: expected {reason:?} in {e}"
        );
    }
    let long = "a".repeat(schema::MAX_NAME_LEN + 1);
    let e = err(&script(&format!("Screenshot(name: \"{long}\")")));
    assert!(e.message.contains("longer than"), "{e}");
    // Sample labels follow the same rules.
    let e = err(&script("Sample(label: \"../x\")"));
    assert!(e.message.contains("path separator"), "{e}");
}

#[test]
fn check_name_rejects_absolute_paths() {
    assert!(schema::check_name("name", "/abs").is_err());
    assert!(schema::check_name("name", "C:\\abs").is_err());
    assert!(schema::check_name("name", "ok_name-1.v2").is_ok());
}

#[test]
fn load_reports_missing_file() {
    let e = ShotScript::load(std::path::Path::new("/nonexistent/shot.ron")).unwrap_err();
    assert_eq!(e.file, "/nonexistent/shot.ron");
    assert!(e.message.contains("cannot read script"), "{e}");
}

#[test]
fn oversized_script_file_is_rejected() {
    let path = std::env::temp_dir().join(format!("shot-script-big-{}.ron", std::process::id()));
    std::fs::write(&path, vec![b' '; schema::MAX_SCRIPT_BYTES as usize + 1]).unwrap();
    let e = ShotScript::load(&path).unwrap_err();
    std::fs::remove_file(&path).ok();
    assert!(e.message.contains("the limit is 1048576 bytes"), "{e}");
}

#[test]
fn too_many_steps_are_rejected() {
    let steps = "WaitSeconds(1.0),".repeat(schema::MAX_STEPS + 1);
    let e = err(&script(&steps));
    assert!(e.message.contains("the limit is 10000"), "{e}");
    let steps = "WaitSeconds(1.0),".repeat(schema::MAX_STEPS);
    ShotScript::parse(&script(&steps), "t.ron").expect("exactly the limit is fine");
}

#[test]
fn duplicate_screenshot_names_ignore_case() {
    let e = err(&script(
        "Screenshot(name: \"Deck_Noon\"), Screenshot(name: \"deck_noon\")",
    ));
    assert!(
        e.message
            .contains("duplicate screenshot name \"deck_noon\""),
        "{e}"
    );
}

#[test]
fn step_lines_skip_block_comments_and_strings() {
    // Line 6 holds a commented-out step of the same kind; line 7 the real one.
    let e = err(&script(
        "        /* WaitSeconds(1.0) */\n        WaitSeconds(0.0),",
    ));
    assert_eq!(e.line, Some(7), "{e}");
    // A multi-line nested block comment and `//` inside a string.
    let e = err(&script(
        "        Sample(label: \"a\"), /* Screenshot(name: \"x\")\n        /* nested */ \
         Screenshot(name: \"y\") */\n        Time(\"noon\"), // Screenshot(name: \"z\")\n        \
         Screenshot(name: \"ok\"),\n        Screenshot(name: \"bad name\"),",
    ));
    assert!(e.message.starts_with("step 4 (Screenshot)"), "{e}");
    assert_eq!(e.line, Some(10), "{e}");
}

/// `TIME_NAMES` must match the `/time` command exactly: the completion list in
/// `common/src/cmd.rs` and the named arms of the server's parser.
#[test]
fn time_names_match_time_command() {
    fn quoted(s: &str) -> Vec<String> {
        s.split('"')
            .skip(1)
            .step_by(2)
            .map(str::to_string)
            .collect()
    }
    let mut ours: Vec<String> = schema::TIME_NAMES.iter().map(|s| s.to_string()).collect();
    ours.sort();

    let common = include_str!("../../../common/src/cmd.rs");
    let list = common
        .split("static ref TIMES: Vec<String> = [")
        .nth(1)
        .expect("TIMES list in common/src/cmd.rs")
        .split(']')
        .next()
        .unwrap();
    let mut common_names = quoted(list);
    common_names.sort();
    assert_eq!(ours, common_names, "TIME_NAMES vs common TIMES");

    let server = include_str!("../../../server/src/cmd.rs");
    let arms = server
        .split("let new_time = match time.as_deref() {")
        .nth(1)
        .expect("time match in server/src/cmd.rs")
        .split("Some(n) =>")
        .next()
        .unwrap();
    let mut server_names: Vec<String> = arms
        .split("Some(\"")
        .skip(1)
        .map(|a| a.split('"').next().unwrap().to_string())
        .collect();
    server_names.sort();
    assert_eq!(ours, server_names, "TIME_NAMES vs server /time arms");
}
