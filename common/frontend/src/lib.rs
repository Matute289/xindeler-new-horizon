mod bounded_writer;
pub use bounded_writer::{BoundedMakeWriter, CompressionGuard, Rotation};

mod lifecycle;
pub use lifecycle::{ErrorDetectorLayer, LogLifecycleManager};

#[cfg(feature = "logging-verbose")]
mod telemetry_layer;
#[cfg(feature = "logging-verbose")]
pub use telemetry_layer::{TelemetryFlushHandle, TelemetryLayer};

#[cfg(not(feature = "tracy"))] use std::fs;
use std::{
    path::Path,
    sync::{Arc, atomic::AtomicBool},
    time::Duration,
};

use termcolor::{ColorChoice, StandardStream};
use tracing::info;
use tracing_appender::non_blocking::WorkerGuard;
use tracing_subscriber::{
    EnvFilter, filter::LevelFilter, fmt::writer::MakeWriter, prelude::*, registry,
};

const RUST_LOG_ENV: &str = "RUST_LOG";

#[cfg(feature = "logging-verbose")]
const CLIENT_INFO_MAX_LINES: u64 = 5_000;
const CLIENT_ERR_MAX_LINES: u64 = 1_000;
#[cfg(feature = "logging-verbose")]
const SERVER_INFO_MAX_LINES: u64 = 10_000;
const SERVER_ERR_MAX_LINES: u64 = 1_000;

const CLIENT_INFO_RETENTION: Duration = Duration::from_secs(24 * 3600);
const CLIENT_ERR_RETENTION: Duration = Duration::from_secs(7 * 24 * 3600);
const SERVER_RETENTION: Duration = Duration::from_secs(30 * 24 * 3600);

/// Holds all log-related guards. Drop order: flush workers before compress
/// thread exits.
pub struct LogGuards {
    pub has_errors: Arc<AtomicBool>,
    pub lifecycle: LogLifecycleManager,
    _worker_guards: Vec<WorkerGuard>,
    _compress_guards: Vec<CompressionGuard>,
    #[cfg(feature = "logging-verbose")]
    telemetry_flush: Option<TelemetryFlushHandle>,
}

impl Drop for LogGuards {
    fn drop(&mut self) {
        #[cfg(feature = "logging-verbose")]
        if let Some(h) = &self.telemetry_flush {
            h.flush();
        }
    }
}

/// Initialise tracing and logging for the logs_path.
///
/// This function will attempt to set up both a file and a terminal logger,
/// falling back to just a terminal logger if the file is unable to be created.
///
/// The logging level is by default set to `INFO`, to change this for any
/// particular crate or module you must use the `RUST_LOG` environment
/// variable.
///
/// For example to set this crate's debug level to `TRACE` you would need the
/// following in your environment.
/// `RUST_LOG="xindeler_voxygen=trace"`
///
/// more complex tracing can be done by concatenating with a `,` as separator:
///  - warn for `prometheus_hyper`, `dot_vox`, `gfx_device_gl::factory,
///    `gfx_device_gl::shade` trace for `xindeler_voxygen`, info for everything
///    else
///
/// `RUST_LOG="prometheus_hyper=warn,dot_vox::parser=warn,gfx_device_gl::
/// factory=warn,gfx_device_gl::shade=warn,xindeler_voxygen=trace,info"`
///
/// By default a few directives are set to `warn` by default, until explicitly
/// overwritten! e.g. `RUST_LOG="gfx_device_gl=debug"`
pub fn init<W2>(
    log_path_file: Option<(&Path, &str)>,
    terminal: &'static W2,
) -> Vec<impl Drop + use<W2>>
where
    W2: MakeWriter<'static> + 'static,
    <W2 as MakeWriter<'static>>::Writer: 'static + Send + Sync,
{
    // To hold the guards that we create, they will cause the logs to be
    // flushed when they're dropped.
    #[cfg(not(feature = "tracy"))]
    let mut guards: Vec<WorkerGuard> = Vec::new();
    #[cfg(feature = "tracy")]
    let guards: Vec<WorkerGuard> = Vec::new();

    // We will do lower logging than the default (INFO) by INCLUSION. This
    // means that if you need lower level logging for a specific module, then
    // put it in the environment in the correct format i.e. DEBUG logging for
    // this crate would be xindeler_voxygen=debug.

    let mut filter = EnvFilter::default().add_directive(LevelFilter::INFO.into());

    let default_directives = [
        "dot_vox::parser=warn",
        "xindeler_common::trade=info",
        "xindeler_world::sim=info",
        "xindeler_world::civ=info",
        "xindeler_world::site::economy=info",
        "xindeler_server::events::entity_manipulation=info",
        "hyper=info",
        "prometheus_hyper=info",
        "mio::poll=info",
        "mio::sys::windows=info",
        "assets_manager::anycache=info",
        "polling::epoll=info",
        "h2=info",
        "tokio_util=info",
        "rustls=info",
        "naga=info",
        "gfx_backend_vulkan=info",
        "wgpu_core=info",
        "wgpu_core::device=warn",
        "wgpu_core::swap_chain=info",
        "xindeler_network_protocol=info",
        "quinn_proto::connection=info",
        "refinery_core::traits::divergent=off",
        "xindeler_server::persistence::character=info",
        "xindeler_server::settings=info",
        "xindeler_query_server=info",
        "symphonia_format_ogg::demuxer=off",
        "symphonia_core::probe=off",
        "wgpu_hal::dx12::device=off",
    ];

    for s in default_directives {
        filter = filter.add_directive(s.parse().unwrap());
    }

    match std::env::var(RUST_LOG_ENV) {
        Ok(env) => {
            for s in env.split(',') {
                match s.parse() {
                    Ok(d) => filter = filter.add_directive(d),
                    Err(err) => eprintln!("WARN ignoring log directive: `{s}`: {err}"),
                }
            }
        },
        Err(std::env::VarError::NotUnicode(os_string)) => {
            eprintln!("WARN ignoring log directives due to non-unicode data: {os_string:?}");
        },
        Err(std::env::VarError::NotPresent) => {},
    };

    let filter = filter; // mutation is done

    let registry = registry();
    #[cfg(not(feature = "tracy"))]
    let mut file_setup = false;
    #[cfg(feature = "tracy")]
    let file_setup = false;
    #[cfg(feature = "tracy")]
    let _terminal = terminal;

    // Create the terminal writer layer.
    #[cfg(feature = "tracy")]
    let registry = registry.with(tracing_tracy::TracyLayer::new(
        tracing_tracy::DefaultConfig::default(),
    ));
    #[cfg(not(feature = "tracy"))]
    let registry = {
        let (non_blocking, stdio_guard) = tracing_appender::non_blocking(terminal.make_writer());
        guards.push(stdio_guard);
        registry.with(tracing_subscriber::fmt::layer().with_writer(non_blocking))
    };

    // Try to create the log file's parent folders.
    #[cfg(not(feature = "tracy"))]
    if let Some((path, file)) = log_path_file {
        match fs::create_dir_all(path) {
            Ok(_) => {
                let file_appender = tracing_appender::rolling::never(path, file); // It is actually rolling daily since the log name is changing daily
                let (non_blocking_file, file_guard) = tracing_appender::non_blocking(file_appender);
                guards.push(file_guard);
                file_setup = true;
                registry
                    .with(tracing_subscriber::fmt::layer().with_writer(non_blocking_file))
                    .with(filter)
                    .init();
            },
            Err(e) => {
                tracing::error!(
                    ?e,
                    "Failed to create log file!. Falling back to terminal logging only.",
                );
                registry.with(filter).init();
            },
        }
    } else {
        registry.with(filter).init();
    }
    #[cfg(feature = "tracy")]
    registry.with(filter).init();

    if file_setup {
        let (path, file) = log_path_file.unwrap();
        info!(?path, ?file, "Setup terminal and file logging.");
    }

    if tracing::level_enabled!(tracing::Level::TRACE) {
        info!("Tracing Level: TRACE");
    } else if tracing::level_enabled!(tracing::Level::DEBUG) {
        info!("Tracing Level: DEBUG");
    };

    // Return the guards
    guards
}

pub fn init_stdout(log_path_file: Option<(&Path, &str)>) -> Vec<impl Drop + use<>> {
    init(log_path_file, &|| StandardStream::stdout(ColorChoice::Auto))
}

/// Initialise the split logging system (replaces `init_stdout()` at call
/// sites):
///  - Terminal output (always)
///  - `{prefix}_err.log` WARN+ERROR (always, daily rotation, 1k lines)
///  - `{prefix}_info.log` DEBUG+ (logging-verbose feature only, hourly, 5k/10k
///    lines)
///  - `{prefix}_telemetry.jsonl` JSON Lines (logging-verbose only, hourly, 20k
///    lines)
///
/// `prefix` should be "client" (voxygen) or "server" (server-cli).
pub fn init_split_logs(prefix: &str, logs_dir: &Path) -> LogGuards {
    use tracing_subscriber::{Layer as _, fmt::layer as fmt_layer};

    let is_server = prefix.starts_with("server");

    // Startup retention cleanup
    let lifecycle = LogLifecycleManager::new(logs_dir.to_owned());
    let (info_ret, err_ret) = if is_server {
        (SERVER_RETENTION, SERVER_RETENTION)
    } else {
        (CLIENT_INFO_RETENTION, CLIENT_ERR_RETENTION)
    };
    lifecycle.cleanup_on_startup(info_ret, err_ret);

    // Build the shared log-level filter (same directives as the existing init())
    let filter = build_split_filter();

    // ErrorDetectorLayer — tracks whether any WARN/ERROR was emitted
    let (error_detector, has_errors) = ErrorDetectorLayer::new();

    // Terminal writer
    let (non_blocking_term, term_guard) =
        tracing_appender::non_blocking(StandardStream::stdout(ColorChoice::Auto));

    // Error file sink (always present)
    let err_max = if is_server {
        SERVER_ERR_MAX_LINES
    } else {
        CLIENT_ERR_MAX_LINES
    };
    let (err_writer, err_compress) =
        BoundedMakeWriter::new(logs_dir, &format!("{prefix}_err"), Rotation::Daily, err_max);

    #[allow(unused_mut)]
    let mut worker_guards: Vec<WorkerGuard> = vec![term_guard];
    #[allow(unused_mut)]
    let mut compress_guards: Vec<CompressionGuard> = vec![err_compress];

    // Optional info sink (logging-verbose only) — Option<L> implements Layer<S>
    #[cfg(feature = "logging-verbose")]
    let info_layer: Option<Box<dyn tracing_subscriber::Layer<_> + Send + Sync>> = {
        let info_max = if is_server {
            SERVER_INFO_MAX_LINES
        } else {
            CLIENT_INFO_MAX_LINES
        };
        let (info_writer, info_compress) = BoundedMakeWriter::new(
            logs_dir,
            &format!("{prefix}_info"),
            Rotation::Hourly,
            info_max,
        );
        compress_guards.push(info_compress);
        Some(
            fmt_layer()
                .with_writer(info_writer)
                .with_filter(LevelFilter::DEBUG)
                .boxed(),
        )
    };
    #[cfg(not(feature = "logging-verbose"))]
    let info_layer: Option<Box<dyn tracing_subscriber::Layer<_> + Send + Sync>> = None;

    // Optional telemetry sink (logging-verbose only)
    #[cfg(feature = "logging-verbose")]
    let (telemetry_layer, telemetry_flush): (
        Option<Box<dyn tracing_subscriber::Layer<_> + Send + Sync>>,
        Option<TelemetryFlushHandle>,
    ) = match TelemetryLayer::new(logs_dir, prefix) {
        Some(t) => {
            let h = t.flush_handle();
            (Some(t.boxed()), Some(h))
        },
        None => (None, None),
    };
    #[cfg(not(feature = "logging-verbose"))]
    let telemetry_layer: Option<Box<dyn tracing_subscriber::Layer<_> + Send + Sync>> = None;

    // Compose full subscriber and initialise
    registry()
        .with(
            fmt_layer()
                .with_writer(non_blocking_term)
                .with_filter(filter),
        )
        .with(
            fmt_layer()
                .with_writer(err_writer)
                .with_filter(LevelFilter::WARN),
        )
        .with(error_detector)
        .with(info_layer)
        .with(telemetry_layer)
        .init();

    LogGuards {
        has_errors,
        lifecycle,
        _worker_guards: worker_guards,
        _compress_guards: compress_guards,
        #[cfg(feature = "logging-verbose")]
        telemetry_flush,
    }
}

fn build_split_filter() -> EnvFilter {
    let mut filter = EnvFilter::default().add_directive(LevelFilter::INFO.into());
    let default_directives = [
        "dot_vox::parser=warn",
        "xindeler_common::trade=info",
        "xindeler_world::sim=info",
        "xindeler_world::civ=info",
        "xindeler_world::site::economy=info",
        "xindeler_server::events::entity_manipulation=info",
        "hyper=info",
        "prometheus_hyper=info",
        "mio::poll=info",
        "mio::sys::windows=info",
        "assets_manager::anycache=info",
        "polling::epoll=info",
        "h2=info",
        "tokio_util=info",
        "rustls=info",
        "naga=info",
        "gfx_backend_vulkan=info",
        "wgpu_core=info",
        "wgpu_core::device=warn",
        "wgpu_core::swap_chain=info",
        "xindeler_network_protocol=info",
        "quinn_proto::connection=info",
        "refinery_core::traits::divergent=off",
        "xindeler_server::persistence::character=info",
        "xindeler_server::settings=info",
        "xindeler_query_server=info",
        "symphonia_format_ogg::demuxer=off",
        "symphonia_core::probe=off",
        "wgpu_hal::dx12::device=off",
    ];
    for s in default_directives {
        filter = filter.add_directive(s.parse().unwrap());
    }
    match std::env::var(RUST_LOG_ENV) {
        Ok(env) => {
            for s in env.split(',') {
                match s.parse() {
                    Ok(d) => filter = filter.add_directive(d),
                    Err(err) => eprintln!("WARN ignoring log directive: `{s}`: {err}"),
                }
            }
        },
        Err(std::env::VarError::NotUnicode(os_string)) => {
            eprintln!("WARN ignoring log directives due to non-unicode data: {os_string:?}");
        },
        Err(std::env::VarError::NotPresent) => {},
    }
    filter
}
