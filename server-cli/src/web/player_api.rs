//! NH-79 Phase 1: `/player_api/v1`, the character-data/rename route family for
//! `xindeler-web-landing`. Deliberately **not** nested under `/ui_api/v1` --
//! different threat model (an authenticated-but-untrusted player, not the
//! trusted admin secret-cookie caller) and no admin capability is reachable
//! from here even in principle (design spec §1.1's "keep it separate" note;
//! plan Phase 1).
//!
//! `validate_bearer_uuid` below is a **Phase 1 debug stub**: it trusts the
//! bearer token verbatim as the caller's uuid, with no signature or expiry
//! check whatsoever. It exists only so this route family can be built and
//! tested end-to-end before `xindeler-auth`'s scoped-token endpoint (spec
//! §6) exists. This game server's web port is loopback-only (NH-75
//! precedent) and sits behind `xindeler-web-api`, which is meant to be the
//! only caller -- but this stub still MUST NOT ship as the final word on
//! auth here. Plan Phase 2 ("Wire the real auth scheme") replaces it with a
//! real call into `xindeler-auth` to validate that token.
//!
//! Because a doc-comment alone doesn't stop a deploy-config slip, `router()`
//! below refuses to build (panics at server startup, not per-request) unless
//! `XINDELER_PLAYER_API_DEBUG_AUTH=1` is set -- a real config/env variable a
//! deployment must explicitly carry, the same class of mistake a missed
//! comment can't catch. Remove this guard's condition (make it unconditional
//! again) only once Phase 2's real `xindeler-auth` validation replaces
//! `validate_bearer_uuid`.

use crate::{
    cli::{Message, MessageReturn},
    web::ui::api::UiRequestSender,
};
use axum::{
    Extension, Json, Router,
    extract::{Path, Request, State},
    http::header::AUTHORIZATION,
    middleware::Next,
    response::{IntoResponse, Response},
    routing::{get, post},
};
use hyper::StatusCode;
use serde::Deserialize;

/// The uuid a Phase-1 debug-stub-authenticated request claims to be. A
/// distinct type (rather than a bare `String` extension) so it can't be
/// confused with some other stringly-typed request extension.
#[derive(Clone)]
struct AuthenticatedUuid(String);

async fn validate_bearer_uuid(mut req: Request, next: Next) -> Result<Response, StatusCode> {
    let uuid = req
        .headers()
        .get(AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|header| header.strip_prefix("Bearer "))
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
        .ok_or(StatusCode::UNAUTHORIZED)?;

    req.extensions_mut().insert(AuthenticatedUuid(uuid));
    Ok(next.run(req).await)
}

/// Required to be set to `1` for this router to build at all -- see the
/// module doc-comment. Checked once at startup, not per-request.
const DEBUG_AUTH_OPT_IN_ENV_VAR: &str = "XINDELER_PLAYER_API_DEBUG_AUTH";

pub fn router(web_ui_request_s: UiRequestSender) -> Router {
    assert_eq!(
        std::env::var(DEBUG_AUTH_OPT_IN_ENV_VAR).as_deref(),
        Ok("1"),
        "/player_api/v1 uses a Phase-1 debug-stub auth middleware that trusts an Authorization: \
         Bearer <uuid> header verbatim, with no real verification. Set \
         {DEBUG_AUTH_OPT_IN_ENV_VAR}=1 to explicitly acknowledge this and start the server anyway \
         (Phase 1 only -- see server-cli/src/web/player_api.rs)."
    );

    Router::new()
        .route("/characters", get(characters))
        .route("/characters/{character_id}/rename", post(rename))
        .layer(axum::middleware::from_fn(validate_bearer_uuid))
        .with_state(web_ui_request_s)
}

async fn characters(
    State(web_ui_request_s): State<UiRequestSender>,
    Extension(AuthenticatedUuid(uuid)): Extension<AuthenticatedUuid>,
) -> Result<impl IntoResponse, StatusCode> {
    let (sender, receiver) = tokio::sync::oneshot::channel();
    let _ = web_ui_request_s
        .send((Message::ListPlayerCharacters { uuid }, sender))
        .await;
    match receiver
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
    {
        MessageReturn::PlayerCharacters(characters) => Ok(Json(characters)),
        _ => Err(StatusCode::INTERNAL_SERVER_ERROR),
    }
}

#[derive(Deserialize)]
struct RenameBody {
    new_alias: String,
}

async fn rename(
    State(web_ui_request_s): State<UiRequestSender>,
    Extension(AuthenticatedUuid(uuid)): Extension<AuthenticatedUuid>,
    Path(character_id): Path<i64>,
    Json(payload): Json<RenameBody>,
) -> Result<impl IntoResponse, (StatusCode, String)> {
    let (sender, receiver) = tokio::sync::oneshot::channel();
    let _ = web_ui_request_s
        .send((
            Message::RenameCharacter {
                uuid,
                character_id,
                new_alias: payload.new_alias,
            },
            sender,
        ))
        .await;
    match receiver
        .await
        .map_err(|_| (StatusCode::INTERNAL_SERVER_ERROR, String::new()))?
    {
        MessageReturn::CharacterRenamed => Ok(StatusCode::NO_CONTENT),
        // The request was understood but refused for a stated reason
        // (not found/not yours, invalid name, name already taken) -- 409,
        // same "understood but refused" precedent `oracle_trigger` already
        // established in `ui/api.rs`, not 500.
        MessageReturn::Error(err) => Err((StatusCode::CONFLICT, err)),
        _ => Err((StatusCode::INTERNAL_SERVER_ERROR, String::new())),
    }
}
