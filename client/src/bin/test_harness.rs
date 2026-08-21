//! Admin test-character harness (ATC-B).
//!
//! A thin headless client that batch-creates/configures **test characters**
//! from a RON roster, driving the server's admin commands — it never touches
//! the save DB. Per roster entry it connects as an admin account, creates the
//! character if it doesn't exist (race → humanoid species at creation), selects
//! it, then issues `/make_test_char <level> [class] [kit]` to configure the
//! rest.
//!
//! Security: this is purely a *client*. The account it logs in as must be a
//! real **admin** on the server (the server gates `/make_test_char` via
//! `needs_role` + `real_role`); a non-admin account is rejected server-side.
//!
//! Run (against a server where `<username>` is an admin):
//! ```text
//! cargo run --bin test_harness --features "bin_bot,tick_network" -- \
//!   --username myadmin --server localhost --roster roster.ron
//! ```

use clap::Parser;
use common::{ViewDistances, clock::Clock, comp};
use serde::Deserialize;
use std::{path::PathBuf, sync::Arc, time::Duration};
use tokio::runtime::Runtime;
use xindeler_client::{Client, ClientType, addr::ConnectionArgs};

/// Safety cap on ticks spent waiting for any single step (~20 s at 30 Hz).
const STEP_TICK_LIMIT: u32 = 600;
/// Ticks to keep running after sending the command so the server applies it.
const FLUSH_TICKS: u32 = 30;

#[derive(Parser)]
#[command(about = "Batch-create/configure admin test characters from a RON roster")]
struct Opt {
    /// Path to the roster RON file.
    #[arg(long, default_value = "roster.ron")]
    roster: PathBuf,
    /// Server hostname.
    #[arg(long, default_value = "localhost")]
    server: String,
    /// Admin account username (must be an admin on the server).
    #[arg(long)]
    username: String,
    /// Admin account password (empty for a `--no-auth` server).
    #[arg(long, default_value = "")]
    password: String,
    /// View distance used when selecting characters.
    #[arg(long, default_value_t = 5)]
    view_distance: u32,
}

#[derive(Debug, Deserialize)]
struct Roster {
    characters: Vec<TestCharSpec>,
}

#[derive(Debug, Deserialize)]
struct TestCharSpec {
    name: String,
    level: u16,
    #[serde(default)]
    class: Option<String>,
    #[serde(default)]
    race: Option<String>,
    #[serde(default)]
    kit: Option<String>,
}

fn species_from_race(race: Option<&str>) -> comp::body::humanoid::Species {
    use comp::body::humanoid::Species;
    match race.map(str::to_lowercase).as_deref() {
        Some("danari") => Species::Danari,
        Some("dwarf") => Species::Dwarf,
        Some("elf") => Species::Elf,
        Some("orc") => Species::Orc,
        Some("draugr") => Species::Draugr,
        // default + explicit "human"
        _ => Species::Human,
    }
}

fn humanoid_body(species: comp::body::humanoid::Species) -> comp::Body {
    comp::body::humanoid::Body {
        species,
        body_type: comp::body::humanoid::BodyType::Male,
        hair_style: 0,
        beard: 0,
        eyes: 0,
        accessory: 0,
        hair_color: 0,
        skin: 0,
        eye_color: 0,
        height_scale: 0,
    }
    .into()
}

/// `/make_test_char` args are positional (`<level> [class] [kit]`); when a kit
/// is given without a class we pass `warrior` as a filler (the creation class)
/// so the kit lands in the right slot.
fn make_test_char_args(spec: &TestCharSpec) -> Vec<String> {
    let mut args = vec![spec.level.to_string()];
    match (&spec.class, &spec.kit) {
        (Some(class), Some(kit)) => {
            args.push(class.clone());
            args.push(kit.clone());
        },
        (Some(class), None) => args.push(class.clone()),
        (None, Some(kit)) => {
            args.push("warrior".to_owned());
            args.push(kit.clone());
        },
        (None, None) => {},
    }
    args
}

fn main() {
    let opt = Opt::parse();
    let _guards = common_frontend::init_stdout(None);

    let roster: Roster = match std::fs::read_to_string(&opt.roster)
        .map_err(|e| e.to_string())
        .and_then(|s| ron::from_str(&s).map_err(|e| e.to_string()))
    {
        Ok(roster) => roster,
        Err(e) => {
            eprintln!("Failed to read roster {:?}: {e}", opt.roster);
            std::process::exit(1);
        },
    };

    let runtime = Arc::new(Runtime::new().expect("Failed to build tokio runtime"));
    println!(
        "test-harness: {} character(s) to set up on {} as '{}'",
        roster.characters.len(),
        opt.server,
        opt.username
    );

    let mut ok = 0usize;
    for spec in &roster.characters {
        match apply_spec(spec, &opt, &runtime) {
            Ok(()) => {
                ok += 1;
                println!(
                    "  ✓ {} — level {}{}{}",
                    spec.name,
                    spec.level,
                    spec.class
                        .as_deref()
                        .map(|c| format!(", class {c}"))
                        .unwrap_or_default(),
                    spec.kit
                        .as_deref()
                        .map(|k| format!(", kit {k}"))
                        .unwrap_or_default(),
                );
            },
            Err(e) => eprintln!("  ✗ {} — {e}", spec.name),
        }
    }
    println!("test-harness: {ok}/{} done.", roster.characters.len());
}

fn apply_spec(spec: &TestCharSpec, opt: &Opt, runtime: &Arc<Runtime>) -> Result<(), String> {
    let addr = ConnectionArgs::Tcp {
        prefer_ipv6: false,
        hostname: opt.server.clone(),
    };
    let mut client = runtime
        .block_on(Client::new(
            addr,
            Arc::clone(runtime),
            &mut None,
            &opt.username,
            &opt.password,
            None,
            |_| false,
            || None,
            &|_| {},
            |_| {},
            PathBuf::new(),
            ClientType::Game,
        ))
        .map_err(|e| format!("connect failed: {e:?}"))?;

    let mut clock = Clock::new(Duration::from_secs_f32(1.0 / 30.0));
    let mut tick = |client: &mut Client| -> Result<(), String> {
        clock.tick();
        client
            .tick_network(clock.real_dt())
            .map_err(|e| format!("network tick failed: {e:?}"))
    };

    // Wait for the character list to load.
    client.load_character_list();
    let mut guard = 0;
    while client.character_list().loading {
        tick(&mut client)?;
        guard += 1;
        if guard > STEP_TICK_LIMIT {
            return Err("timed out loading character list".to_owned());
        }
    }

    // The server gates the command, but fail fast with a clear message if the
    // account isn't an admin.
    if !client.is_moderator() {
        return Err(format!(
            "account '{}' is not an admin on this server",
            opt.username
        ));
    }

    let find_id = |client: &Client| {
        client
            .character_list()
            .characters
            .iter()
            .find(|c| c.character.alias == spec.name)
            .and_then(|c| c.character.id)
    };

    // Find the character by name, or create it (race → species at creation; the
    // real class is applied by /make_test_char, so we always create a Warrior with
    // the always-valid sword starter).
    let char_id = if let Some(id) = find_id(&client) {
        id
    } else {
        client.create_character(
            spec.name.clone(),
            Some("common.items.weapons.sword.starter".into()),
            None,
            humanoid_body(species_from_race(spec.race.as_deref())),
            false,
            None,
            comp::class::ClassKind::Warrior,
            // BL-33: bots start True Neutral.
            comp::Ethos::default(),
            // BL-31: bots start with no background.
            comp::Background::default(),
        );
        client.load_character_list();
        guard = 0;
        loop {
            tick(&mut client)?;
            if let Some(id) = find_id(&client) {
                break id;
            }
            guard += 1;
            if guard > STEP_TICK_LIMIT {
                return Err("timed out creating character".to_owned());
            }
        }
    };

    // Select the character and wait until in-game.
    client.request_character(char_id, ViewDistances {
        terrain: opt.view_distance,
        entity: opt.view_distance,
    });
    guard = 0;
    while client.position().is_none() {
        tick(&mut client)?;
        guard += 1;
        if guard > STEP_TICK_LIMIT {
            return Err("timed out entering the game".to_owned());
        }
    }

    // Configure via the admin command, then keep ticking so the server applies it.
    client.send_command("make_test_char".to_owned(), make_test_char_args(spec));
    for _ in 0..FLUSH_TICKS {
        tick(&mut client)?;
    }

    Ok(())
}
