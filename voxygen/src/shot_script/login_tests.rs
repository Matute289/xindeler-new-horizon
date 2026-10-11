use super::{ShotScript, login::*};
use common::character::CharacterId;
use std::{
    collections::HashMap,
    ffi::OsString,
    net::{Ipv4Addr, Ipv6Addr, SocketAddr},
    path::PathBuf,
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
            ("T13SHOT", Some(CharacterId(1)), false),
            (CHARACTER_ALIAS, Some(CharacterId(9)), false),
            (CHARACTER_ALIAS, Some(CharacterId(4)), false),
        ]),
        CharAction::Play(CharacterId(4))
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
