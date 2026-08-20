use crate::cli::{Message, MessageReturn, OracleTarget, Shutdown};
use axum::{
    Json, Router,
    extract::{ConnectInfo, Query, Request, State},
    http::header::COOKIE,
    middleware::Next,
    response::{IntoResponse, Response},
    routing::{get, post},
};
use hyper::StatusCode;
use serde::{Deserialize, Serialize};
use std::{
    collections::HashSet,
    net::{IpAddr, SocketAddr},
    sync::Arc,
};
use tokio::sync::Mutex;

/// Keep Size small, so we dont have to Clone much for each request.
#[derive(Clone)]
struct UiApiToken {
    secret_token: String,
}

pub(crate) type UiRequestSender =
    tokio::sync::mpsc::Sender<(Message, tokio::sync::oneshot::Sender<MessageReturn>)>;

#[derive(Clone, Default)]
struct IpAddresses {
    users: Arc<Mutex<HashSet<IpAddr>>>,
}

async fn validate_secret(
    State(token): State<UiApiToken>,
    req: Request,
    next: Next,
) -> Result<Response, StatusCode> {
    let session_cookie = req.headers().get(COOKIE).ok_or(StatusCode::UNAUTHORIZED)?;

    pub const X_SECRET_TOKEN: &str = "X-Secret-Token";
    let expected = format!("{X_SECRET_TOKEN}={}", token.secret_token);

    if session_cookie.as_bytes() != expected.as_bytes() {
        return Err(StatusCode::UNAUTHORIZED);
    }

    Ok(next.run(req).await)
}

/// Logs each new IP address that accesses this API authenticated
async fn log_users(
    State(ip_addresses): State<IpAddresses>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    req: Request,
    next: Next,
) -> Result<Response, StatusCode> {
    let mut ip_addresses = ip_addresses.users.lock().await;
    let ip_addr = addr.ip();
    if !ip_addresses.contains(&ip_addr) {
        ip_addresses.insert(ip_addr);
        let users_so_far = ip_addresses.len();
        tracing::info!(?ip_addr, ?users_so_far, "Is accessing the /ui_api endpoint");
    }
    Ok(next.run(req).await)
}

//TODO: do security audit before we extend this api with more security relevant
// functionality (e.g. account management)
pub fn router(web_ui_request_s: UiRequestSender, secret_token: String) -> Router {
    let token = UiApiToken { secret_token };
    let ip_addrs = IpAddresses::default();
    Router::new()
        .route("/players", get(players))
        .route("/logs", get(logs))
        .route("/send_global_msg", post(send_global_msg))
        .route("/info", get(info))
        .route("/shutdown", post(shutdown))
        .route("/disconnect_all", post(disconnect_all))
        .route("/chronicle", get(chronicle))
        .route("/oracle/events", get(oracle_events))
        .route("/oracle/trigger", post(oracle_trigger))
        .route("/oracle/enabled", post(oracle_enabled))
        .layer(axum::middleware::from_fn_with_state(ip_addrs, log_users))
        .layer(axum::middleware::from_fn_with_state(token, validate_secret))
        .with_state(web_ui_request_s)
}

async fn players(
    State(web_ui_request_s): State<UiRequestSender>,
) -> Result<impl IntoResponse, StatusCode> {
    let (sender, receiver) = tokio::sync::oneshot::channel();
    let _ = web_ui_request_s.send((Message::ListPlayers, sender)).await;
    match receiver
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
    {
        MessageReturn::Players(players) => Ok(Json(players)),
        _ => Err(StatusCode::INTERNAL_SERVER_ERROR),
    }
}

async fn logs(
    State(web_ui_request_s): State<UiRequestSender>,
) -> Result<impl IntoResponse, StatusCode> {
    let (sender, receiver) = tokio::sync::oneshot::channel();
    let _ = web_ui_request_s.send((Message::ListLogs, sender)).await;
    match receiver
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
    {
        MessageReturn::Logs(logs) => Ok(Json(logs)),
        _ => Err(StatusCode::INTERNAL_SERVER_ERROR),
    }
}

#[derive(Deserialize)]
struct SendWorldMsgBody {
    msg: String,
}

async fn send_global_msg(
    State(web_ui_request_s): State<UiRequestSender>,
    Json(payload): Json<SendWorldMsgBody>,
) -> Result<impl IntoResponse, StatusCode> {
    let (dummy_s, _) = tokio::sync::oneshot::channel();
    let _ = web_ui_request_s
        .send((Message::SendGlobalMsg { msg: payload.msg }, dummy_s))
        .await;
    Ok(())
}

async fn info(
    State(web_ui_request_s): State<UiRequestSender>,
) -> Result<impl IntoResponse, StatusCode> {
    let (sender, receiver) = tokio::sync::oneshot::channel();
    let _ = web_ui_request_s.send((Message::ServerInfo, sender)).await;
    match receiver
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
    {
        MessageReturn::Info(info) => Ok(Json(info)),
        _ => Err(StatusCode::INTERNAL_SERVER_ERROR),
    }
}

/// Mirrors `crate::cli::Shutdown`'s three modes as a JSON body instead of a
/// clap subcommand -- `graceful` takes `seconds`/`reason`, the other two
/// ignore both fields.
#[derive(Deserialize)]
#[serde(rename_all = "snake_case")]
enum ShutdownMode {
    Graceful,
    Immediate,
    Cancel,
}

#[derive(Deserialize)]
struct ShutdownBody {
    mode: ShutdownMode,
    #[serde(default)]
    seconds: u64,
    reason: Option<String>,
}

async fn shutdown(
    State(web_ui_request_s): State<UiRequestSender>,
    Json(payload): Json<ShutdownBody>,
) -> Result<impl IntoResponse, StatusCode> {
    let command = match payload.mode {
        ShutdownMode::Graceful => Shutdown::Graceful {
            seconds: payload.seconds,
            reason: payload
                .reason
                .unwrap_or_else(|| "The server is shutting down".to_owned()),
        },
        ShutdownMode::Immediate => Shutdown::Immediate,
        ShutdownMode::Cancel => Shutdown::Cancel,
    };
    // `Message::Shutdown` never sends a response on any of its three arms
    // (a graceful/cancel mutates `ShutdownCoordinator` state with nothing to
    // report back; an immediate shutdown exits the process) -- same
    // fire-and-forget shape as `send_global_msg` above.
    let (dummy_s, _) = tokio::sync::oneshot::channel();
    let _ = web_ui_request_s
        .send((Message::Shutdown { command }, dummy_s))
        .await;
    Ok(())
}

async fn disconnect_all(
    State(web_ui_request_s): State<UiRequestSender>,
) -> Result<impl IntoResponse, StatusCode> {
    let (dummy_s, _) = tokio::sync::oneshot::channel();
    let _ = web_ui_request_s
        .send((Message::DisconnectAllClients, dummy_s))
        .await;
    Ok(())
}

#[derive(Deserialize)]
struct ChronicleQuery {
    limit: usize,
}

async fn chronicle(
    State(web_ui_request_s): State<UiRequestSender>,
    Query(query): Query<ChronicleQuery>,
) -> Result<impl IntoResponse, StatusCode> {
    let (sender, receiver) = tokio::sync::oneshot::channel();
    let _ = web_ui_request_s
        .send((Message::ListChronicle { limit: query.limit }, sender))
        .await;
    match receiver
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
    {
        MessageReturn::Chronicle(entries) => Ok(Json(entries)),
        _ => Err(StatusCode::INTERNAL_SERVER_ERROR),
    }
}

async fn oracle_events(
    State(web_ui_request_s): State<UiRequestSender>,
) -> Result<impl IntoResponse, StatusCode> {
    let (sender, receiver) = tokio::sync::oneshot::channel();
    let _ = web_ui_request_s
        .send((Message::OracleListEvents, sender))
        .await;
    match receiver
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
    {
        MessageReturn::OracleEvents(events) => Ok(Json(events)),
        _ => Err(StatusCode::INTERNAL_SERVER_ERROR),
    }
}

/// Mirrors `crate::cli::OracleTarget` as a JSON body.
#[derive(Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum OracleTargetBody {
    Player { alias: String },
    Coords { x: f32, y: f32, z: f32 },
}

impl From<OracleTargetBody> for OracleTarget {
    fn from(body: OracleTargetBody) -> Self {
        match body {
            OracleTargetBody::Player { alias } => OracleTarget::Player { alias },
            OracleTargetBody::Coords { x, y, z } => OracleTarget::Coords { x, y, z },
        }
    }
}

#[derive(Deserialize)]
struct OracleTriggerBody {
    event_id: String,
    target: OracleTargetBody,
    #[serde(default)]
    dry_run: bool,
    /// The caller (the gateway) is responsible for only setting this after
    /// its own step-up re-auth -- this route accepts the flag verbatim, it
    /// does not itself verify that re-auth happened.
    #[serde(default)]
    high_impact_override: bool,
}

/// Mirrors `MessageReturn::{OracleTriggered,OraclePreview}` as one tagged
/// JSON shape.
#[derive(Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum OracleTriggerResponse {
    Triggered {
        event_id: String,
        at: [f32; 3],
        requested: usize,
        spawned: usize,
        clamped: bool,
    },
    Preview {
        event_id: String,
        at: [f32; 3],
        requested: usize,
        spawned: usize,
        clamped: bool,
        bodies: Vec<String>,
        distance_to_nearest_player: Option<f32>,
    },
}

async fn oracle_trigger(
    State(web_ui_request_s): State<UiRequestSender>,
    Json(payload): Json<OracleTriggerBody>,
) -> Result<impl IntoResponse, (StatusCode, String)> {
    let (sender, receiver) = tokio::sync::oneshot::channel();
    let _ = web_ui_request_s
        .send((
            Message::OracleTrigger {
                event_id: payload.event_id,
                target: payload.target.into(),
                dry_run: payload.dry_run,
                high_impact_override: payload.high_impact_override,
            },
            sender,
        ))
        .await;
    match receiver
        .await
        .map_err(|_| (StatusCode::INTERNAL_SERVER_ERROR, String::new()))?
    {
        MessageReturn::OracleTriggered {
            event_id,
            at,
            requested,
            spawned,
            clamped,
        } => Ok(Json(OracleTriggerResponse::Triggered {
            event_id,
            at,
            requested,
            spawned,
            clamped,
        })),
        MessageReturn::OraclePreview {
            event_id,
            at,
            requested,
            spawned,
            clamped,
            bodies,
            distance_to_nearest_player,
        } => Ok(Json(OracleTriggerResponse::Preview {
            event_id,
            at,
            requested,
            spawned,
            clamped,
            bodies,
            distance_to_nearest_player,
        })),
        // The request was understood but refused for a stated reason
        // (unknown event, rate limit, cooldown, ceiling, per-event cap, or
        // the kill switch) -- 409, not 500: nothing broke, the trigger was
        // just refused.
        MessageReturn::Error(err) => Err((StatusCode::CONFLICT, err)),
        _ => Err((StatusCode::INTERNAL_SERVER_ERROR, String::new())),
    }
}

#[derive(Deserialize)]
struct OracleEventsEnabledBody {
    enabled: bool,
}

async fn oracle_enabled(
    State(web_ui_request_s): State<UiRequestSender>,
    Json(payload): Json<OracleEventsEnabledBody>,
) -> Result<impl IntoResponse, StatusCode> {
    let (dummy_s, _) = tokio::sync::oneshot::channel();
    let _ = web_ui_request_s
        .send((
            Message::OracleEventsEnabled {
                enabled: payload.enabled,
            },
            dummy_s,
        ))
        .await;
    Ok(())
}
