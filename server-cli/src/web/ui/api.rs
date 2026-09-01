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
        .route("/send_targeted_msg", post(send_targeted_msg))
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

#[derive(Deserialize)]
struct SendTargetedMsgBody {
    target_uuids: Vec<String>,
    operator_uuid: String,
    msg: String,
}

#[derive(Serialize)]
struct SendTargetedMsgResponse {
    delivered_to: Vec<String>,
    not_found: Vec<String>,
}

/// Upper bound on how many uuids one `send_targeted_msg` call may name.
///
/// The engine side resolves every entry against the connected-player set on
/// the tick-owning thread, so an unbounded array is a direct stall of the game
/// loop for every player -- reachable by anyone holding only the shared
/// `/ui_api/v1` secret. A few hundred comfortably covers any real ops use
/// (the alternative for a genuinely server-wide announcement is
/// `send_global_msg`, which costs one broadcast rather than N lookups).
const MAX_TARGETED_MSG_UUIDS: usize = 256;

/// The cap itself, split out from the handler so it can be tested directly:
/// `server-cli` has no HTTP test harness (no `tower` dev-dependency), and this
/// returns the exact `(StatusCode, String)` pair the handler hands back.
fn check_targeted_msg_batch(target_count: usize) -> Result<(), (StatusCode, String)> {
    if target_count > MAX_TARGETED_MSG_UUIDS {
        // Understood but refused, for a stated reason -- the same 400
        // precedent the malformed-uuid case below uses, not 500.
        return Err((
            StatusCode::BAD_REQUEST,
            format!(
                "target_uuids may name at most {MAX_TARGETED_MSG_UUIDS} uuids (got \
                 {target_count}); use send_global_msg for a server-wide announcement"
            ),
        ));
    }
    Ok(())
}

/// Sends `msg` to exactly `target_uuids` (skipping any not currently
/// connected -- see `MessageReturn::TargetedMsgSent`). Same auth tier as
/// `send_global_msg` above (the shared secret only, no operator/role check);
/// unlike the kick/ban/unban routes this isn't a moderation action.
///
/// `operator_uuid` is deliberately unverified -- it is an audit record, not an
/// authorization check. For it to be worth anything it has to be correlatable,
/// so every call logs it together with the caller's source IP and the delivery
/// outcome. The `log_users` middleware is not a substitute: it logs an IP once,
/// the first time that IP is ever seen on `/ui_api` at all, so a forged
/// `operator_uuid` on this route would otherwise have nothing to cross-check
/// against.
async fn send_targeted_msg(
    State(web_ui_request_s): State<UiRequestSender>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    Json(payload): Json<SendTargetedMsgBody>,
) -> Result<impl IntoResponse, (StatusCode, String)> {
    let ip_addr = addr.ip();
    let target_count = payload.target_uuids.len();

    if let Err(rejection) = check_targeted_msg_batch(target_count) {
        tracing::info!(
            ?ip_addr,
            operator_uuid = %payload.operator_uuid,
            target_count,
            "targeted message rejected: too many target_uuids"
        );
        return Err(rejection);
    }

    let (sender, receiver) = tokio::sync::oneshot::channel();
    // Kept for the audit log below; the request itself takes ownership.
    let operator_uuid = payload.operator_uuid.clone();
    let _ = web_ui_request_s
        .send((
            Message::SendTargetedMsg {
                target_uuids: payload.target_uuids,
                operator_uuid: payload.operator_uuid,
                msg: payload.msg,
            },
            sender,
        ))
        .await;
    match receiver
        .await
        .map_err(|_| (StatusCode::INTERNAL_SERVER_ERROR, String::new()))?
    {
        MessageReturn::TargetedMsgSent {
            delivered_to,
            not_found,
        } => {
            tracing::info!(
                ?ip_addr,
                %operator_uuid,
                target_count,
                delivered = delivered_to.len(),
                not_found = not_found.len(),
                "targeted message sent"
            );
            Ok(Json(SendTargetedMsgResponse {
                delivered_to,
                not_found,
            }))
        },
        // A malformed uuid in `target_uuids` -- the request was understood
        // but rejected for a stated reason, same "understood but refused"
        // precedent `kick_player` below established, not 500.
        MessageReturn::Error(err) => {
            tracing::info!(
                ?ip_addr,
                %operator_uuid,
                target_count,
                error = %err,
                "targeted message rejected"
            );
            Err((StatusCode::BAD_REQUEST, err))
        },
        _ => Err((StatusCode::INTERNAL_SERVER_ERROR, String::new())),
    }
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
/// account. Refuses the request if `character_id` doesn't actually belong to
/// `uuid` (see `cmd::admin_suspend_character`'s doc comment), rather than
/// silently acting on whatever account it really belongs to. `duration_secs`
/// is required: `0` means permanent (i.e. lasts until a manual unsuspend),
/// so an omitted value can never silently mean "forever".
#[derive(Deserialize)]
struct AdminSuspendBody {
    operator_uuid: String,
    reason: String,
    duration_secs: u64,
}

async fn suspend_character(
    State(web_ui_request_s): State<UiRequestSender>,
    Path((uuid, character_id)): Path<(String, i64)>,
    Json(payload): Json<AdminSuspendBody>,
) -> Result<impl IntoResponse, (StatusCode, String)> {
    let (sender, receiver) = tokio::sync::oneshot::channel();
    let _ = web_ui_request_s
        .send((
            Message::AdminSuspendCharacter {
                target_uuid: uuid,
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
    Path((uuid, character_id)): Path<(String, i64)>,
    Json(payload): Json<AdminUnsuspendBody>,
) -> Result<impl IntoResponse, (StatusCode, String)> {
    let (sender, receiver) = tokio::sync::oneshot::channel();
    let _ = web_ui_request_s
        .send((
            Message::AdminUnsuspendCharacter {
                target_uuid: uuid,
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

#[cfg(test)]
mod targeted_msg_cap_tests {
    use super::*;

    /// `server-cli` carries no HTTP test harness (no `tower` dev-dependency),
    /// so these exercise the admission check the handler delegates to -- it
    /// returns the exact `(StatusCode, String)` pair the handler hands back to
    /// the caller.
    #[test]
    fn an_oversized_batch_is_rejected_with_400() {
        let (status, body) = check_targeted_msg_batch(MAX_TARGETED_MSG_UUIDS + 1)
            .expect_err("a batch past the cap must be refused");
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert!(
            body.contains(&MAX_TARGETED_MSG_UUIDS.to_string()),
            "the rejection must tell the caller what the cap is, got {body:?}"
        );
    }

    #[test]
    fn a_batch_at_the_cap_is_accepted() {
        assert!(
            check_targeted_msg_batch(MAX_TARGETED_MSG_UUIDS).is_ok(),
            "the cap itself is inclusive"
        );
    }

    #[test]
    fn ordinary_batches_are_accepted() {
        for count in [0, 1, 2, 32] {
            assert!(
                check_targeted_msg_batch(count).is_ok(),
                "{count} targets is a perfectly ordinary request"
            );
        }
    }
}
