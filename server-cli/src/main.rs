#![deny(unsafe_code)]
#![deny(clippy::clone_on_ref_ptr)]

#[cfg(all(
    target_os = "windows",
    not(feature = "hot-agent"),
    not(feature = "hot-site"),
))]
#[global_allocator]
static GLOBAL: mimalloc::MiMalloc = mimalloc::MiMalloc;

/// `server-cli` interface commands not to be confused with the commands sent
/// from the client to the server
mod cli;
mod settings;
mod shutdown_coordinator;
mod tui_runner;
mod tuilog;
mod web;
use crate::{
    cli::{
        Admin, ArgvApp, ArgvCommand, BenchParams, CharacterSummaryDto, LocationDto, Message,
        MessageReturn, OracleEventsDto, OracleTarget, ServerInfoDto, SharedCommand, Shutdown,
    },
    settings::Settings,
    shutdown_coordinator::ShutdownCoordinator,
    tui_runner::Tui,
    tuilog::TuiLog,
};
use common::{
    character::CharacterId,
    clock::Clock,
    comp::{ChatType, Player, Pos},
    consts::MIN_RECOMMENDED_TOKIO_THREADS,
};
use common_base::span;
use core::sync::atomic::{AtomicUsize, Ordering};
use rand::distr::SampleString;
use server::{
    Event, Input, Server, oracle,
    oracle::policy::{self, PolicyError},
    persistence::DatabaseSettings,
    settings::Protocol,
};
use std::{
    io,
    sync::{Arc, atomic::AtomicBool},
    time::{Duration, Instant},
};
use tokio::sync::Notify;
use tracing::{error, info, trace};
use vek::Vec3;

lazy_static::lazy_static! {
    pub static ref LOG: TuiLog<'static> = TuiLog::default();
}
const TPS: u64 = 30;

/// Recovers a loaded `DmEvent`'s own `<id>` from the path it was ingested
/// from -- the inverse of the `{event_id}.dmevent.ron`/`.json` match
/// `server::oracle::trigger::find_loaded_event` does in the other
/// direction. `OracleEvents` keys its table by path, not id, since a path is
/// what the filesystem watcher actually observes.
fn dmevent_id_from_path(path: &std::path::Path) -> Option<String> {
    let name = path.file_name()?.to_str()?;
    name.strip_suffix(".dmevent.ron")
        .or_else(|| name.strip_suffix(".dmevent.json"))
        .map(str::to_owned)
}

/// Renders a `server::oracle::policy::PolicyError` as an operator-facing
/// message for `MessageReturn::Error`.
fn render_policy_error(err: &PolicyError) -> String {
    use server::oracle::trigger::TriggerError;
    match err {
        PolicyError::Disabled => {
            "ORACLE triggers are currently disabled (kill switch is off).".to_owned()
        },
        PolicyError::RateLimited { window, limit } => {
            format!("Refused: the {limit}-per-{window} trigger rate limit has been reached.")
        },
        PolicyError::Cooldown {
            event_id,
            remaining,
        } => format!(
            "Refused: '{event_id}' fired too recently, {}s of cooldown remain.",
            remaining.as_secs()
        ),
        PolicyError::Trigger(TriggerError::UnknownEvent { event_id }) => {
            format!("No loaded ORACLE event named '{event_id}'.")
        },
        PolicyError::Trigger(TriggerError::NoTemplatesMatched { event_id }) => format!(
            "Oracle event '{event_id}' names entity templates, but none of them are currently \
             loaded."
        ),
        PolicyError::Trigger(TriggerError::WouldExceedCeiling {
            live,
            planned,
            ceiling,
        }) => format!(
            "Refused: {live} ORACLE entities are already live, this trigger would add {planned} \
             more, exceeding the ceiling of {ceiling}."
        ),
        PolicyError::Trigger(TriggerError::WouldExceedPerEventCap { requested, cap }) => format!(
            "Refused: this trigger's resolved plan of {requested} exceeds the per-trigger cap of \
             {cap}."
        ),
    }
}

fn main() -> io::Result<()> {
    #[cfg(feature = "tracy")]
    common_base::tracy_client::Client::start();

    use clap::Parser;
    let app = ArgvApp::parse();

    let basic = !app.tui || app.command.is_some();
    let noninteractive = app.non_interactive;
    let no_auth = app.no_auth;
    let sql_log_mode = app.sql_log_mode;

    // noninteractive implies basic
    let basic = basic || noninteractive;

    let shutdown_signal = Arc::new(AtomicBool::new(false));

    let server_logs_dir = common_base::userdata_dir().join("server").join("logs");
    let _log_guards = common_frontend::init_split_logs("server", &server_logs_dir);

    // Load settings
    let settings = settings::Settings::load().ok_or(io::ErrorKind::Other)?;

    #[cfg(not(target_os = "windows"))]
    {
        for signal in &settings.shutdown_signals {
            let _ = signal_hook::flag::register(signal.to_signal(), Arc::clone(&shutdown_signal));
        }
    }

    #[cfg(target_os = "windows")]
    if !settings.shutdown_signals.is_empty() {
        tracing::warn!(
            "Server configuration contains shutdown signals, but your platform does not support \
             them"
        );
    }

    // Determine folder to save server data in
    let server_data_dir = {
        let mut path = common_base::userdata_dir();
        info!("Using userdata folder at {}", path.display());
        path.push(server::DEFAULT_DATA_DIR_NAME);
        path
    };

    // We don't need that many threads in the async pool, at least 2 but generally
    // 25% of all available will do
    // TODO: evaluate std::thread::available_concurrency as a num_cpus replacement
    let runtime = Arc::new(
        tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .worker_threads((num_cpus::get() / 4).max(MIN_RECOMMENDED_TOKIO_THREADS))
            .thread_name_fn(|| {
                static ATOMIC_ID: AtomicUsize = AtomicUsize::new(0);
                let id = ATOMIC_ID.fetch_add(1, Ordering::SeqCst);
                format!("tokio-server-{}", id)
            })
            .build()
            .unwrap(),
    );

    #[cfg(feature = "hot-agent")]
    {
        agent::init();
    }
    #[cfg(feature = "hot-site")]
    {
        world::init();
    }

    // Load server settings
    let mut server_settings = server::Settings::load(&server_data_dir);
    let mut editable_settings = server::EditableSettings::load(&server_data_dir);

    // Apply no_auth modifier to the settings
    if no_auth {
        server_settings.auth_server_address = None;
        server_settings.auth_service_address = None;
    }

    // Relative to data_dir
    const PERSISTENCE_DB_DIR: &str = "saves";

    let database_settings = DatabaseSettings {
        db_dir: server_data_dir.join(PERSISTENCE_DB_DIR),
        sql_log_mode,
    };

    let mut bench = None;
    if let Some(command) = app.command {
        match command {
            ArgvCommand::Shared(SharedCommand::Admin { command }) => {
                let login_provider = server::login_provider::LoginProvider::new(
                    server_settings
                        .auth_service_address
                        .clone()
                        .or_else(|| server_settings.auth_server_address.clone()),
                    runtime,
                )
                .map_err(io::Error::other)?;

                return match command {
                    Admin::Add { username, role } => {
                        // FIXME: Currently the UUID can get returned even if the file didn't
                        // change, so this can't be relied on as an error
                        // code; moreover, we do nothing with the UUID
                        // returned in the success case.  Fix the underlying function to return
                        // enough information that we can reliably return an error code.
                        let _ = server::add_admin(
                            &username,
                            role,
                            &login_provider,
                            &mut editable_settings,
                            &server_data_dir,
                        );
                        Ok(())
                    },
                    Admin::Remove { username } => {
                        // FIXME: Currently the UUID can get returned even if the file didn't
                        // change, so this can't be relied on as an error
                        // code; moreover, we do nothing with the UUID
                        // returned in the success case.  Fix the underlying function to return
                        // enough information that we can reliably return an error code.
                        let _ = server::remove_admin(
                            &username,
                            &login_provider,
                            &mut editable_settings,
                            &server_data_dir,
                        );
                        Ok(())
                    },
                };
            },
            ArgvCommand::Bench(params) => {
                bench = Some(params);
                // If we are trying to benchmark, don't limit the server view distance.
                server_settings.max_view_distance = None;
                // TODO: add setting to adjust wildlife spawn density, note I
                // tried but Index setup makes it a bit
                // annoying, might require a more involved refactor to get
                // working nicely
            },
        };
    }

    // Panic hook to ensure that console mode is set back correctly if in non-basic
    // mode
    if !basic {
        let hook = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            Tui::shutdown(basic);
            hook(info);
        }));
    }

    let tui = (!noninteractive).then(|| Tui::run(basic));

    info!("Starting server...");

    let protocols_and_addresses = server_settings.gameserver_protocols.clone();
    let web_port = &settings.web_address.port();
    // Create server
    #[cfg_attr(not(feature = "worldgen"), expect(unused_mut))]
    let mut server = Server::new(
        server_settings,
        editable_settings,
        database_settings,
        &server_data_dir,
        &|_| {},
        Arc::clone(&runtime),
    )
    .expect("Failed to create server instance!");

    let registry = Arc::clone(server.metrics_registry());
    let chat = server.chat_cache().clone();
    let metrics_shutdown = Arc::new(Notify::new());
    let metrics_shutdown_clone = Arc::clone(&metrics_shutdown);
    let web_chat_secret = settings.web_chat_secret.clone();
    let ui_api_secret = settings.ui_api_secret.clone().unwrap_or_else(|| {
        // when no secret is provided we generate one that we distribute via the /ui
        // endpoint
        use rand::distr::Alphanumeric;
        Alphanumeric.sample_string(&mut rand::rng(), 32)
    });

    let (web_ui_request_s, web_ui_request_r) = tokio::sync::mpsc::channel(1000);

    runtime.spawn(async move {
        web::run(
            registry,
            chat,
            web_chat_secret,
            ui_api_secret,
            web_ui_request_s,
            settings.web_address,
            metrics_shutdown_clone.notified(),
        )
        .await
    });

    // Collect addresses that the server is listening to log.
    let gameserver_addresses = protocols_and_addresses
        .into_iter()
        .map(|protocol| match protocol {
            Protocol::Tcp { address } => ("TCP", address),
            Protocol::Quic {
                address,
                cert_file_path: _,
                key_file_path: _,
            } => ("QUIC", address),
        });

    info!(
        ?web_port,
        ?gameserver_addresses,
        "Server is ready to accept connections."
    );

    #[cfg(feature = "worldgen")]
    if let Some(bench) = bench {
        server.create_centered_persister(bench.view_distance);
    }

    server_loop(
        server,
        bench,
        settings,
        tui,
        web_ui_request_r,
        shutdown_signal,
    )?;

    metrics_shutdown.notify_one();

    Ok(())
}

fn server_loop(
    mut server: Server,
    bench: Option<BenchParams>,
    settings: Settings,
    tui: Option<Tui>,
    mut web_ui_request_r: tokio::sync::mpsc::Receiver<(
        Message,
        tokio::sync::oneshot::Sender<MessageReturn>,
    )>,
    shutdown_signal: Arc<AtomicBool>,
) -> io::Result<()> {
    // Set up an fps clock
    let mut clock = Clock::new(Duration::from_secs_f64(1.0 / TPS as f64));
    let mut shutdown_coordinator = ShutdownCoordinator::new(Arc::clone(&shutdown_signal));
    let mut bench_exit_time = None;

    let mut tick_no = 0u64;
    'outer: loop {
        span!(guard, "work");
        if let Some(bench) = bench {
            if let Some(t) = bench_exit_time {
                if Instant::now() > t {
                    break;
                }
            } else if tick_no != 0 && !server.chunks_pending() {
                println!("Chunk loading complete");
                bench_exit_time = Some(Instant::now() + Duration::from_secs(bench.duration.into()));
            }
        };

        tick_no += 1;
        // Terminate the server if instructed to do so by the shutdown coordinator
        if shutdown_coordinator.check(&mut server, &settings) {
            break;
        }

        let events = server
            .tick(Input::default(), clock.game_dt())
            .expect("Failed to tick server");

        for event in events {
            match event {
                Event::ClientConnected { entity: _ } => info!("Client connected!"),
                Event::ClientDisconnected { entity: _ } => info!("Client disconnected!"),
                Event::Chat { entity: _, msg } => info!("[Client] {}", msg),
            }
        }

        // Clean up the server after a tick.
        server.cleanup();

        if tick_no.rem_euclid(1000) == 0 {
            trace!(?tick_no, "keepalive")
        }

        let mut handle_msg = |msg, response: tokio::sync::oneshot::Sender<MessageReturn>| {
            use specs::{Join, WorldExt};
            match msg {
                Message::Shutdown {
                    command: Shutdown::Cancel,
                } => shutdown_coordinator.abort_shutdown(&mut server),
                Message::Shutdown {
                    command: Shutdown::Graceful { seconds, reason },
                } => {
                    shutdown_coordinator.initiate_shutdown(
                        &mut server,
                        Duration::from_secs(seconds),
                        reason,
                    );
                },
                Message::Shutdown {
                    command: Shutdown::Immediate,
                } => {
                    return true;
                },
                Message::Shared(SharedCommand::Admin {
                    command: Admin::Add { username, role },
                }) => {
                    server.add_admin(&username, role);
                },
                Message::Shared(SharedCommand::Admin {
                    command: Admin::Remove { username },
                }) => {
                    server.remove_admin(&username);
                },
                #[cfg(feature = "worldgen")]
                Message::LoadArea { view_distance } => {
                    server.create_centered_persister(view_distance);
                },
                Message::SqlLogMode { mode } => {
                    server.set_sql_log_mode(mode);
                },
                Message::DisconnectAllClients => {
                    server.disconnect_all_clients();
                },
                Message::ListPlayers => {
                    let players: Vec<String> = server
                        .state()
                        .ecs()
                        .read_storage::<Player>()
                        .join()
                        .map(|p| p.alias.clone())
                        .collect();
                    let _ = response.send(MessageReturn::Players(players));
                },
                Message::ListLogs => {
                    let log = LOG.inner.lock().unwrap();
                    let lines: Vec<_> = log
                        .lines
                        .iter()
                        .rev()
                        .take(30)
                        .map(|l| l.to_string())
                        .collect();
                    let _ = response.send(MessageReturn::Logs(lines));
                },
                Message::SendGlobalMsg { msg } => {
                    use server::state_ext::StateExt;
                    let msg = ChatType::Meta.into_plain_msg(msg);
                    server.state().send_chat(msg, false);
                },
                Message::ServerInfo => {
                    let player_count = server.state().ecs().read_storage::<Player>().join().count();
                    let info = ServerInfoDto {
                        version: common::util::DISPLAY_VERSION.clone(),
                        player_count,
                        shutdown_pending_secs: shutdown_coordinator.pending_shutdown_secs(),
                    };
                    let _ = response.send(MessageReturn::Info(info));
                },
                Message::ListChronicle { limit } => {
                    let ecs = server.state().ecs();
                    let all: Vec<String> = ecs
                        .read_resource::<oracle::ChronicleLog>()
                        .iter()
                        .map(str::to_owned)
                        .collect();
                    let start = all.len().saturating_sub(limit);
                    let _ = response.send(MessageReturn::Chronicle(all[start..].to_vec()));
                },
                Message::OracleListEvents => {
                    let ecs = server.state().ecs();
                    let watcher = ecs.read_resource::<oracle::OracleWatcher>();
                    let dm_events: Vec<String> = watcher
                        .events()
                        .dm_events()
                        .filter_map(|(path, _)| dmevent_id_from_path(path))
                        .collect();
                    let entity_templates: Vec<String> = watcher
                        .events()
                        .entity_templates()
                        .map(|(_, template)| template.entity_template_id.clone())
                        .collect();
                    drop(watcher);
                    let _ = response.send(MessageReturn::OracleEvents(OracleEventsDto {
                        dm_events,
                        entity_templates,
                    }));
                },
                Message::OracleTrigger {
                    event_id,
                    target,
                    dry_run,
                    high_impact_override,
                } => {
                    let ecs = server.state().ecs();
                    let pos_result = match target {
                        OracleTarget::Coords { x, y, z } => Ok(Vec3::new(x, y, z)),
                        OracleTarget::Player { alias } => {
                            (&ecs.read_storage::<Player>(), &ecs.read_storage::<Pos>())
                                .join()
                                .find(|(p, _)| p.alias == alias)
                                .map(|(_, pos)| pos.0)
                                .ok_or_else(|| format!("player '{alias}' is not online"))
                        },
                    };

                    match pos_result {
                        Err(err) => {
                            let _ = response.send(MessageReturn::Error(err));
                        },
                        Ok(pos) => match policy::gated_trigger_dm_event(
                            ecs,
                            &event_id,
                            pos,
                            dry_run,
                            high_impact_override,
                        ) {
                            Ok(outcome) => {
                                let at = [outcome.at.x, outcome.at.y, outcome.at.z];
                                let event_id = outcome.event_id.to_string();
                                if dry_run {
                                    let distance_to_nearest_player =
                                        (&ecs.read_storage::<Player>(), &ecs.read_storage::<Pos>())
                                            .join()
                                            .map(|(_, p)| p.0.distance(pos))
                                            .fold(None, |acc: Option<f32>, d| {
                                                Some(acc.map_or(d, |a| a.min(d)))
                                            });
                                    let _ = response.send(MessageReturn::OraclePreview {
                                        event_id,
                                        at,
                                        requested: outcome.requested,
                                        spawned: outcome.spawned,
                                        clamped: outcome.clamped,
                                        bodies: outcome.bodies,
                                        distance_to_nearest_player,
                                    });
                                } else {
                                    let _ = response.send(MessageReturn::OracleTriggered {
                                        event_id,
                                        at,
                                        requested: outcome.requested,
                                        spawned: outcome.spawned,
                                        clamped: outcome.clamped,
                                    });
                                }
                            },
                            Err(err) => {
                                let _ =
                                    response.send(MessageReturn::Error(render_policy_error(&err)));
                            },
                        },
                    }
                },
                Message::OracleEventsEnabled { enabled } => {
                    *server
                        .state()
                        .ecs()
                        .write_resource::<policy::OracleEventsEnabled>() =
                        policy::OracleEventsEnabled(enabled);
                },
                Message::ListPlayerCharacters { uuid } => {
                    match server.list_player_characters(&uuid) {
                        Ok(characters) => {
                            let characters = characters
                                .into_iter()
                                .map(|c| CharacterSummaryDto {
                                    character_id: c.character_id.0,
                                    name: c.alias,
                                    level: u32::from(c.level),
                                    class: c.class,
                                    location: c.location.map(|site| LocationDto {
                                        site,
                                        kingdom: None,
                                        continent: None,
                                    }),
                                })
                                .collect();
                            let _ = response.send(MessageReturn::PlayerCharacters(characters));
                        },
                        Err(err) => {
                            error!(%err, "list_player_characters failed");
                            let _ = response.send(MessageReturn::Error(err.public_message()));
                        },
                    }
                },
                Message::RenameCharacter {
                    uuid,
                    character_id,
                    new_alias,
                } => match server.rename_character(&uuid, CharacterId(character_id), &new_alias) {
                    Ok(()) => {
                        let _ = response.send(MessageReturn::CharacterRenamed);
                    },
                    Err(err) => {
                        error!(%err, "rename_character failed");
                        let _ = response.send(MessageReturn::Error(err.public_message()));
                    },
                },
            }
            false
        };

        if let Some(tui) = tui.as_ref() {
            while let Ok(msg) = tui.msg_r.try_recv() {
                let (sender, mut recv) = tokio::sync::oneshot::channel();
                if handle_msg(msg, sender) {
                    info!("Closing the server");
                    break 'outer;
                }
                if let Ok(msg_answ) = recv.try_recv() {
                    match msg_answ {
                        MessageReturn::Players(players) => info!("Players: {:?}", players),
                        MessageReturn::Logs(_) => info!("skipp sending logs to tui"),
                        MessageReturn::Info(info) => info!(?info, "Server info"),
                        MessageReturn::Chronicle(entries) => {
                            info!(count = entries.len(), "Chronicle tail")
                        },
                        MessageReturn::OracleEvents(events) => info!(
                            dm_events = ?events.dm_events,
                            entity_templates = ?events.entity_templates,
                            "Loaded ORACLE assets"
                        ),
                        MessageReturn::OracleTriggered {
                            event_id, spawned, ..
                        } => info!(%event_id, spawned, "Oracle event triggered"),
                        MessageReturn::OraclePreview {
                            event_id, spawned, ..
                        } => info!(%event_id, spawned, "Oracle event preview"),
                        MessageReturn::Error(err) => error!(%err, "Command failed"),
                        MessageReturn::PlayerCharacters(characters) => {
                            info!(count = characters.len(), "Listed player characters")
                        },
                        MessageReturn::CharacterRenamed => info!("Character renamed"),
                    };
                }
            }
        }

        while let Ok((msg, sender)) = web_ui_request_r.try_recv() {
            if handle_msg(msg, sender) {
                info!("Closing the server");
                break 'outer;
            }
        }

        drop(guard);
        // Wait for the next tick.
        clock.tick();
        #[cfg(feature = "tracy")]
        common_base::tracy_client::frame_mark();
    }
    Ok(())
}
