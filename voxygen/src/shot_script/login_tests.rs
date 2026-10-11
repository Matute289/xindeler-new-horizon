use super::{ShotScript, login::*};
use common::character::CharacterId;
use std::{
    collections::HashMap,
    ffi::OsString,
    net::{Ipv4Addr, Ipv6Addr, SocketAddr},
    path::PathBuf,
    time::{Duration, Instant},
};

const EXAMPLE: &str = include_str!("../../shot-script/example.ron");

fn ok(value: &str) -> SocketAddr {
    parse_loopback_server(value).unwrap_or_else(|e| panic!("{value:?} should be accepted: {e}"))
}

fn refused(value: &str) -> SetupError {
    parse_loopback_server(value).expect_err("address should be refused")
}

#[test]
fn loopback_addresses_are_accepted() {
    assert_eq!(
        ok("127.0.0.1:14004"),
        SocketAddr::from((Ipv4Addr::LOCALHOST, 14004))
    );
    assert_eq!(ok("127.1.2.3:1"), SocketAddr::from(([127, 1, 2, 3], 1)));
    assert_eq!(
        ok("[::1]:14004"),
        SocketAddr::from((Ipv6Addr::LOCALHOST, 14004))
    );
    assert_eq!(ok("[::ffff:127.0.0.1]:14004").port(), 14004);
    // `localhost` maps to 127.0.0.1 without any lookup.
    assert_eq!(
        ok("localhost:14004"),
        SocketAddr::from((Ipv4Addr::LOCALHOST, 14004))
    );
    assert_eq!(ok("LocalHost:65535").port(), 65535);
}

#[test]
fn non_loopback_addresses_are_refused() {
    for value in [
        "10.0.0.5:14004",
        "0.0.0.0:14004",
        "192.168.1.2:14004",
        "8.8.8.8:14004",
        "[::]:14004",
        "[2001:db8::1]:14004",
        "[::ffff:10.0.0.5]:14004",
        "example.com:14004",
        "greenmountain.dev:14004",
        "127.0.0.1.evil.com:14004",
        "localhost.evil.com:14004",
        "localhost.:14004",
        "evil-localhost:14004",
    ] {
        assert!(
            matches!(refused(value), SetupError::NotLoopback { .. }),
            "{value:?} must be refused as non-loopback"
        );
    }
}

#[test]
fn malformed_addresses_are_refused() {
    for value in [
        "",
        " 127.0.0.1:14004",
        "127.0.0.1:14004 ",
        "127.0.0.1",
        "localhost",
        "localhost:",
        "localhost:0",
        "127.0.0.1:0",
        "localhost:99999",
        "localhost:port",
        ":14004",
        "::1:14004",
        "::1",
    ] {
        assert!(
            matches!(refused(value), SetupError::BadServer { .. }),
            "{value:?} must be refused as malformed"
        );
    }
}

#[test]
fn refusal_message_names_the_variable() {
    let msg = refused("example.com:14004").to_string();
    assert!(
        msg.contains(SERVER_ENV) && msg.contains("loopback"),
        "{msg}"
    );
}

/// A fresh, unique directory under the system temp dir (not created).
fn scratch(tag: &str) -> PathBuf {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    std::env::temp_dir().join(format!("shot-login-{tag}-{}-{nanos}", std::process::id()))
}

#[test]
fn out_dir_is_created_owner_only() {
    let root = scratch("create");
    let dir = root.join("a").join("b");
    let got = prepare_out_dir(dir.as_os_str()).expect("should create the directory");
    assert_eq!(got, dir);
    assert!(dir.is_dir());
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(&dir).unwrap().permissions().mode() & 0o777;
        assert_eq!(
            mode & 0o077,
            0,
            "group/other must have no access, got {mode:o}"
        );
    }
    // An existing directory is accepted as is.
    prepare_out_dir(dir.as_os_str()).expect("existing directory is fine");
    std::fs::remove_dir_all(&root).unwrap();
}

#[test]
fn out_dir_rejects_relative_parent_and_files() {
    for raw in ["relative/out", "out", "/tmp/../etc/shots"] {
        assert!(
            matches!(
                prepare_out_dir(raw.as_ref()),
                Err(SetupError::OutDir { .. })
            ),
            "{raw:?} must be refused"
        );
    }
    let root = scratch("file");
    std::fs::create_dir_all(&root).unwrap();
    let file = root.join("not-a-dir");
    std::fs::write(&file, b"x").unwrap();
    let err = prepare_out_dir(file.as_os_str()).expect_err("a file is not a directory");
    assert!(err.to_string().contains("not a directory"), "{err}");
    std::fs::remove_dir_all(&root).unwrap();
}

#[cfg(unix)]
#[test]
fn out_dir_rejects_symlink() {
    let root = scratch("link");
    let target = root.join("target");
    std::fs::create_dir_all(&target).unwrap();
    let link = root.join("link");
    std::os::unix::fs::symlink(&target, &link).unwrap();
    let err = prepare_out_dir(link.as_os_str()).expect_err("a symlink must be refused");
    assert!(err.to_string().contains("symlink"), "{err}");
    std::fs::remove_dir_all(&root).unwrap();
}

fn env(pairs: &[(&str, &str)]) -> impl Fn(&str) -> Option<OsString> {
    let map: HashMap<String, OsString> = pairs
        .iter()
        .map(|(k, v)| (k.to_string(), OsString::from(v)))
        .collect();
    move |k| map.get(k).cloned()
}

fn example() -> ShotScript { ShotScript::parse(EXAMPLE, "example.ron").unwrap() }

#[test]
fn setup_requires_server_and_out() {
    let out = scratch("setup");
    let out_s = out.to_str().unwrap();
    assert!(matches!(
        setup_with(Ok(example()), env(&[(OUT_ENV, out_s)])),
        Err(SetupError::MissingEnv(SERVER_ENV))
    ));
    assert!(matches!(
        setup_with(Ok(example()), env(&[(SERVER_ENV, ""), (OUT_ENV, out_s)])),
        Err(SetupError::MissingEnv(SERVER_ENV))
    ));
    assert!(matches!(
        setup_with(Ok(example()), env(&[(SERVER_ENV, "127.0.0.1:14004")])),
        Err(SetupError::MissingEnv(OUT_ENV))
    ));
    // The server is checked before the output directory is touched.
    assert!(matches!(
        setup_with(
            Ok(example()),
            env(&[(SERVER_ENV, "example.com:14004"), (OUT_ENV, out_s)])
        ),
        Err(SetupError::NotLoopback { .. })
    ));
    assert!(!out.exists());

    let setup = setup_with(
        Ok(example()),
        env(&[(SERVER_ENV, "localhost:14004"), (OUT_ENV, out_s)]),
    )
    .expect("valid setup");
    assert_eq!(setup.server, SocketAddr::from((Ipv4Addr::LOCALHOST, 14004)));
    assert_eq!(setup.out_dir, out);
    std::fs::remove_dir_all(&out).unwrap();
}

#[test]
fn setup_reports_script_errors_first() {
    let err = ShotScript::parse("not ron", "bad.ron").unwrap_err();
    assert!(matches!(
        setup_with(Err(err), env(&[])),
        Err(SetupError::Script(_))
    ));
}

#[cfg(unix)]
#[test]
fn setup_rejects_non_unicode_server() {
    use std::os::unix::ffi::OsStringExt;
    let var = |k: &str| (k == SERVER_ENV).then(|| OsString::from_vec(vec![0xff, b':', b'1']));
    assert!(matches!(
        setup_with(Ok(example()), var),
        Err(SetupError::NotUnicode(SERVER_ENV))
    ));
}

#[test]
fn char_select_creates_then_plays() {
    let mut d = CharSelectDriver::default();
    // Still loading: nothing to do.
    assert_eq!(d.decide(true, []), CharAction::Wait);
    // Loaded, ours missing: create once.
    assert_eq!(
        d.decide(false, [("Other", Some(CharacterId(1)), false)]),
        CharAction::Create
    );
    assert_eq!(d.decide(false, []), CharAction::Wait);
    // Not yet saved (no id): keep waiting.
    assert_eq!(
        d.decide(false, [(CHARACTER_ALIAS, None, false)]),
        CharAction::Wait
    );
    assert_eq!(
        d.decide(false, [(CHARACTER_ALIAS, Some(CharacterId(7)), false)]),
        CharAction::Play(CharacterId(7))
    );
    // Entering the world is requested only once.
    assert_eq!(
        d.decide(false, [(CHARACTER_ALIAS, Some(CharacterId(7)), false)]),
        CharAction::Wait
    );
}

#[test]
fn char_select_reuses_existing_character() {
    let mut d = CharSelectDriver::default();
    assert_eq!(
        d.decide(false, [
            ("Other", Some(CharacterId(1)), false),
            (CHARACTER_ALIAS, Some(CharacterId(9)), false),
            (CHARACTER_ALIAS, Some(CharacterId(4)), false),
        ]),
        CharAction::Play(CharacterId(4))
    );
}

#[test]
fn char_select_matches_alias_ignoring_case() {
    // A server that normalises aliases must not trigger a second creation.
    let mut d = CharSelectDriver::default();
    assert_eq!(
        d.decide(false, [
            ("T13Shot", Some(CharacterId(3)), false),
            ("t13shot2", Some(CharacterId(1)), false),
        ]),
        CharAction::Play(CharacterId(3))
    );
}

#[test]
fn char_select_fails_on_suspended_character() {
    let mut d = CharSelectDriver::default();
    assert!(matches!(
        d.decide(false, [(CHARACTER_ALIAS, Some(CharacterId(2)), true)]),
        CharAction::Fail(_)
    ));
}

#[test]
fn loopback_lookalikes_are_refused() {
    // Numeric shorthands that some resolvers treat as 127.0.0.1, userinfo,
    // bracketed IPv4/hostnames and zone ids: all refused, nothing resolved.
    for value in [
        "127.1:14004",
        "2130706433:14004",
        "0x7f.0.0.1:14004",
        "0177.0.0.1:14004",
        "127.0.0.1@evil.com:14004",
        "evil.com@127.0.0.1:14004",
        "[127.0.0.1]:14004",
        "[localhost]:14004",
        "[::ffff:127.0.0.1%1]:14004",
        "[::1%1]:14004",
        "[::ffff:127.0.0.1%lo0]:14004",
        "[::1]",
        "[::1]:",
        "[[::1]]:14004",
        "localhost:14004/path",
        "http://127.0.0.1:14004",
    ] {
        assert!(
            parse_loopback_server(value).is_err(),
            "{value:?} must be refused"
        );
    }
    assert!(matches!(
        refused("[::ffff:127.0.0.1%1]:14004"),
        SetupError::NotLoopback { .. }
    ));
}

#[test]
fn connect_timeout_parsing() {
    use std::ffi::OsStr;
    assert_eq!(
        parse_connect_timeout(None).unwrap(),
        Duration::from_secs(DEFAULT_CONNECT_TIMEOUT_S)
    );
    assert_eq!(
        parse_connect_timeout(Some(OsStr::new(""))).unwrap(),
        Duration::from_secs(DEFAULT_CONNECT_TIMEOUT_S)
    );
    assert_eq!(
        parse_connect_timeout(Some(OsStr::new("20"))).unwrap(),
        Duration::from_secs(20)
    );
    for bad in ["0", "-1", "1.5", "20s", " 20", "601", "abc"] {
        assert!(
            matches!(
                parse_connect_timeout(Some(OsStr::new(bad))),
                Err(SetupError::BadTimeout { .. })
            ),
            "{bad:?} must be refused"
        );
    }
}

#[test]
fn setup_rejects_bad_timeout_before_touching_out_dir() {
    let out = scratch("timeout");
    let out_s = out.to_str().unwrap();
    assert!(matches!(
        setup_with(
            Ok(example()),
            env(&[
                (SERVER_ENV, "127.0.0.1:14004"),
                (CONNECT_TIMEOUT_ENV, "0"),
                (OUT_ENV, out_s)
            ])
        ),
        Err(SetupError::BadTimeout { .. })
    ));
    assert!(!out.exists());
}

#[test]
fn menu_connects_once_then_waits_or_aborts() {
    let server = SocketAddr::from((Ipv4Addr::LOCALHOST, 14004));
    let timeout = Duration::from_secs(20);
    let t0 = Instant::now();
    let decide = |started, now, idle, connecting| {
        menu_decide(
            started,
            now,
            idle,
            connecting,
            server,
            timeout,
            Some("boom"),
        )
    };
    assert_eq!(decide(None, t0, true, false), MenuAction::Connect(server));
    // Still connecting inside the deadline.
    assert_eq!(
        decide(Some(t0), t0 + Duration::from_secs(19), false, true),
        MenuAction::Wait
    );
    // Past the deadline: abort with a clear message.
    match decide(Some(t0), t0 + Duration::from_secs(21), false, true) {
        MenuAction::Abort(msg) => assert!(msg.contains("scratch server"), "{msg}"),
        other => panic!("expected abort, got {other:?}"),
    }
    // Connected (pipelines being built): the deadline no longer applies.
    assert_eq!(
        decide(Some(t0), t0 + Duration::from_secs(300), false, false),
        MenuAction::Wait
    );
    // Back to idle after an attempt: abort with the menu's message.
    assert_eq!(
        decide(Some(t0), t0, true, false),
        MenuAction::Abort("boom".into())
    );
}

#[test]
fn owner_and_mode_rules() {
    assert_eq!(owner_mode_problem(501, 0o700, 501), None);
    assert_eq!(owner_mode_problem(501, 0o755, 501), None);
    assert!(owner_mode_problem(0, 0o700, 501).is_some());
    assert!(owner_mode_problem(501, 0o770, 501).is_some());
    assert!(owner_mode_problem(501, 0o707, 501).is_some());
    assert!(owner_mode_problem(501, 0o1777, 501).is_some());
}

#[cfg(unix)]
#[test]
fn existing_out_dir_must_not_be_group_or_world_writable() {
    use std::os::unix::fs::PermissionsExt;
    let dir = scratch("mode");
    std::fs::create_dir_all(&dir).unwrap();
    for (mode, accepted) in [(0o700, true), (0o755, true), (0o775, false), (0o777, false)] {
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(mode)).unwrap();
        let got = prepare_out_dir(dir.as_os_str());
        assert_eq!(got.is_ok(), accepted, "mode {mode:o}: {got:?}");
        if let Err(e) = got {
            assert!(e.to_string().contains("group or others"), "{e}");
        }
    }
    // The owner probe leaves nothing behind.
    assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 0);
    std::fs::remove_dir_all(&dir).unwrap();
}
