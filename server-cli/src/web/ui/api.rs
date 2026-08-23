use crate::cli::{Message, MessageReturn, OracleTarget, Shutdown};
use axum::{
    Json, Router,
    extract::{ConnectInfo, Path, Query, Request, State},
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

// Account- and character-management routes below (kick/ban/unban, character
// lookup, per-character suspend/unsuspend) are the security-relevant
// functionality this comment used to flag as a future audit item -- they've
// now had that pass: each wraps engine capability that already existed as a
// chat command, attribution is cross-checked against the server's own
// admin/moderator roster (not just this shared secret) via `real_role`, and
// every write is scoped to a single uuid (or uuid + character id) path param
// with no broader admin-list-management surface exposed. See the design
// doc's investigation for the full reasoning.
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
        .route("/players/{uuid}/kick", post(kick_player))
        .route("/players/{uuid}/ban", post(ban_player))
        .route("/players/{uuid}/unban", post(unban_player))
        .route("/players/{uuid}/characters", get(player_characters))
        .route(
            "/players/{uuid}/characters/{character_id}/suspend",
            post(suspend_character),
        )
        .route(
            "/players/{uuid}/characters/{character_id}/unsuspend",
            post(unsuspend_character),
        )
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

/// `operator_uuid` identifies the acting admin/moderator's own Xindeler
/// account -- checked server-side against the admin/moderator roster
/// (`real_role`), not just trusted verbatim because this request carried the
/// shared secret.
#[derive(Deserialize)]
struct AdminKickBody {
    operator_uuid: String,
    reason: Option<String>,
}

async fn kick_player(
    State(web_ui_request_s): State<UiRequestSender>,
    Path(uuid): Path<String>,
    Json(payload): Json<AdminKickBody>,
) -> Result<impl IntoResponse, (StatusCode, String)> {
    let (sender, receiver) = tokio::sync::oneshot::channel();
    let _ = web_ui_request_s
        .send((
            Message::AdminKickPlayer {
                target_uuid: uuid,
                operator_uuid: payload.operator_uuid,
                reason: payload.reason,
            },
            sender,
        ))
        .await;
    match receiver
        .await
        .map_err(|_| (StatusCode::INTERNAL_SERVER_ERROR, String::new()))?
    {
        MessageReturn::AdminActionOk { .. } => Ok(StatusCode::OK),
        // The request was understood but refused for a stated reason
        // (operator not registered, target not connected, operator doesn't
        // outrank target) -- 409, same "understood but refused" precedent
        // `oracle_trigger` above already established, not 500.
        MessageReturn::Error(err) => Err((StatusCode::CONFLICT, err)),
        _ => Err((StatusCode::INTERNAL_SERVER_ERROR, String::new())),
    }
}

/// `duration_secs` omitted or `null` means a permanent ban, matching
/// `Banlist::ban_operation`'s own `end_date: Option<...>` semantics.
/// `overwrite` defaults to `false`, same default the equivalent chat command
/// uses. `target_username`, if the caller has it, avoids a server-side
/// network round-trip to resolve one (see
/// `server::cmd::make_ban_info_for_uuid`'s doc comment) -- purely
/// informational (stored in the ban record for display), falls back to the
/// bare uuid if omitted.
#[derive(Deserialize)]
struct AdminBanBody {
    operator_uuid: String,
    #[serde(default)]
    target_username: Option<String>,
    reason: String,
    #[serde(default)]
    duration_secs: Option<u64>,
    #[serde(default)]
    overwrite: bool,
}

async fn ban_player(
    State(web_ui_request_s): State<UiRequestSender>,
    Path(uuid): Path<String>,
    Json(payload): Json<AdminBanBody>,
) -> Result<impl IntoResponse, (StatusCode, String)> {
    let (sender, receiver) = tokio::sync::oneshot::channel();
    let _ = web_ui_request_s
        .send((
            Message::AdminBanPlayer {
                target_uuid: uuid,
                operator_uuid: payload.operator_uuid,
                target_username: payload.target_username,
                reason: payload.reason,
                duration_secs: payload.duration_secs,
                overwrite: payload.overwrite,
            },
            sender,
        ))
        .await;
    match receiver
        .await
        .map_err(|_| (StatusCode::INTERNAL_SERVER_ERROR, String::new()))?
    {
        // Carries the resulting ban record (reason, expiry) straight
        // through so the caller gets immediate confirmation of what was
        // actually persisted, without a separate read route.
        MessageReturn::AdminActionOk { ban } => Ok(Json(ban)),
        MessageReturn::Error(err) => Err((StatusCode::CONFLICT, err)),
        _ => Err((StatusCode::INTERNAL_SERVER_ERROR, String::new())),
    }
}

#[derive(Deserialize)]
struct AdminUnbanBody {
    operator_uuid: String,
    #[serde(default)]
    target_username: Option<String>,
}

async fn unban_player(
    State(web_ui_request_s): State<UiRequestSender>,
    Path(uuid): Path<String>,
    Json(payload): Json<AdminUnbanBody>,
) -> Result<impl IntoResponse, (StatusCode, String)> {
    let (sender, receiver) = tokio::sync::oneshot::channel();
    let _ = web_ui_request_s
        .send((
            Message::AdminUnbanPlayer {
                target_uuid: uuid,
                operator_uuid: payload.operator_uuid,
                target_username: payload.target_username,
            },
            sender,
        ))
        .await;
    match receiver
        .await
        .map_err(|_| (StatusCode::INTERNAL_SERVER_ERROR, String::new()))?
    {
        MessageReturn::AdminActionOk { .. } => Ok(StatusCode::OK),
        MessageReturn::Error(err) => Err((StatusCode::CONFLICT, err)),
        _ => Err((StatusCode::INTERNAL_SERVER_ERROR, String::new())),
    }
}

/// Freezes a single character, leaving the rest of `uuid`'s account
/// untouched -- unlike `/players/{uuid}/ban`, which acts on the whole
/// account. `duration_secs` is required: `0` means permanent (i.e. lasts
/// until a manual unsuspend), so an omitted value can never silently mean
/// "forever".
#[derive(Deserialize)]
struct AdminSuspendBody {
    operator_uuid: String,
    reason: String,
    duration_secs: u64,
}

async fn suspend_character(
    State(web_ui_request_s): State<UiRequestSender>,
    Path((_uuid, character_id)): Path<(String, i64)>,
    Json(payload): Json<AdminSuspendBody>,
) -> Result<impl IntoResponse, (StatusCode, String)> {
    let (sender, receiver) = tokio::sync::oneshot::channel();
    let _ = web_ui_request_s
        .send((
            Message::AdminSuspendCharacter {
                character_id,
                operator_uuid: payload.operator_uuid,
                reason: payload.reason,
                duration_secs: payload.duration_secs,
            },
            sender,
        ))
        .await;
    match receiver
        .await
        .map_err(|_| (StatusCode::INTERNAL_SERVER_ERROR, String::new()))?
    {
        MessageReturn::AdminActionOk { .. } => Ok(StatusCode::OK),
        MessageReturn::Error(err) => Err((StatusCode::CONFLICT, err)),
        _ => Err((StatusCode::INTERNAL_SERVER_ERROR, String::new())),
    }
}

#[derive(Deserialize)]
struct AdminUnsuspendBody {
    operator_uuid: String,
}

async fn unsuspend_character(
    State(web_ui_request_s): State<UiRequestSender>,
    Path((_uuid, character_id)): Path<(String, i64)>,
    Json(payload): Json<AdminUnsuspendBody>,
) -> Result<impl IntoResponse, (StatusCode, String)> {
    let (sender, receiver) = tokio::sync::oneshot::channel();
    let _ = web_ui_request_s
        .send((
            Message::AdminUnsuspendCharacter {
                character_id,
                operator_uuid: payload.operator_uuid,
            },
            sender,
        ))
        .await;
    match receiver
        .await
        .map_err(|_| (StatusCode::INTERNAL_SERVER_ERROR, String::new()))?
    {
        MessageReturn::AdminActionOk { .. } => Ok(StatusCode::OK),
        MessageReturn::Error(err) => Err((StatusCode::CONFLICT, err)),
        _ => Err((StatusCode::INTERNAL_SERVER_ERROR, String::new())),
    }
}

/// Admin-scoped character lookup for an arbitrary `uuid` -- deliberately
/// separate from `/player_api/v1/characters`, which only ever resolves the
/// bearer-token-authenticated caller's own uuid. Reuses the same
/// `CharacterSummaryDto`/`LocationDto` shape; only the `Message` variant
/// differs, so this DTO layer never has to know which auth path a request
/// came through.
async fn player_characters(
    State(web_ui_request_s): State<UiRequestSender>,
    Path(uuid): Path<String>,
) -> Result<impl IntoResponse, StatusCode> {
    let (sender, receiver) = tokio::sync::oneshot::channel();
    let _ = web_ui_request_s
        .send((Message::AdminListPlayerCharacters { uuid }, sender))
        .await;
    match receiver
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
    {
        MessageReturn::PlayerCharacters(characters) => Ok(Json(characters)),
        _ => Err(StatusCode::INTERNAL_SERVER_ERROR),
    }
}
