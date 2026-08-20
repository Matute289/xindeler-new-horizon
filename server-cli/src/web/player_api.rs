//! NH-79: `/player_api/v1`, the character-data/rename route family for
//! `xindeler-web-landing`. Deliberately **not** nested under `/ui_api/v1` --
//! different threat model (an authenticated-but-untrusted player, not the
//! trusted admin secret-cookie caller) and no admin capability is reachable
//! from here even in principle (design spec §1.1's "keep it separate" note).
//!
//! `validate_bearer_uuid` below (Phase 2) redeems the caller's
//! `Authorization: Bearer <CharacterAccessToken>` against `xindeler-auth`'s
//! `/verify-character-access-token`, using the same `AUTH_SERVICE_TOKEN`-
//! backed `authc::AuthClient` `login_provider::LoginProvider` already holds
//! for player logins -- the game server's *existing* credential, never a new
//! one (N-01's own design: only the game server ever redeems a token, using
//! the credential it already holds for exactly this class of interaction).
//! If no auth server is configured (`--no-auth`), every request here is
//! refused outright rather than falling back to a weaker check.

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
use server::authc::{AuthClient, AuthClientError, CharacterAccessToken};
use std::sync::Arc;

/// The uuid a request's `CharacterAccessToken` redeemed to. A distinct type
/// (rather than a bare `String` extension) so it can't be confused with some
/// other stringly-typed request extension.
#[derive(Clone)]
struct AuthenticatedUuid(String);

fn bearer_token(req: &Request) -> Option<&str> {
    req.headers()
        .get(AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|header| header.strip_prefix("Bearer "))
        .filter(|s| !s.is_empty())
}

async fn validate_bearer_uuid(
    State(auth_client): State<Option<Arc<AuthClient>>>,
    mut req: Request,
    next: Next,
) -> Result<Response, StatusCode> {
    let token: CharacterAccessToken = bearer_token(&req)
        .ok_or(StatusCode::UNAUTHORIZED)?
        .parse()
        .map_err(|_| StatusCode::UNAUTHORIZED)?;

    // `--no-auth` (no auth server configured): refuse outright, never accept
    // an unverified token -- there is no weaker-but-still-safe fallback for
    // this route family.
    let auth_client = auth_client.ok_or(StatusCode::SERVICE_UNAVAILABLE)?;

    // `authc::AuthClient` is blocking (reqwest::blocking); running it
    // directly here would stall this worker's whole async reactor for the
    // duration of the HTTPS round-trip to xindeler-auth.
    let uuid =
        tokio::task::spawn_blocking(move || auth_client.verify_character_access_token(token))
            .await
            .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
            .map_err(|err| match err {
                // xindeler-auth explicitly rejected the token (missing,
                // expired, already-redeemed) -- the caller's fault.
                AuthClientError::ServerError(..) => StatusCode::UNAUTHORIZED,
                // A misconfiguration or transient failure on this game
                // server's own side (e.g. AUTH_SERVICE_TOKEN unset/rotated,
                // xindeler-auth unreachable, a malformed response) --
                // distinct from an actually-invalid token so it shows up in
                // logs/metrics as a 5xx instead of masquerading as "every
                // player's token is invalid".
                _ => StatusCode::INTERNAL_SERVER_ERROR,
            })?;

    req.extensions_mut()
        .insert(AuthenticatedUuid(uuid.to_string()));
    Ok(next.run(req).await)
}

pub fn router(web_ui_request_s: UiRequestSender, auth_client: Option<Arc<AuthClient>>) -> Router {
    Router::new()
        .route("/characters", get(characters))
        .route("/characters/{character_id}/rename", post(rename))
        .layer(axum::middleware::from_fn_with_state(
            auth_client,
            validate_bearer_uuid,
        ))
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

#[cfg(test)]
mod tests {
    use super::bearer_token;
    use axum::extract::Request;

    fn request_with_header(value: Option<&str>) -> Request {
        let mut builder = Request::builder().uri("/characters");
        if let Some(value) = value {
            builder = builder.header("Authorization", value);
        }
        builder.body(axum::body::Body::empty()).unwrap()
    }

    #[test]
    fn extracts_the_token_from_a_well_formed_bearer_header() {
        let req = request_with_header(Some("Bearer abc123"));
        assert_eq!(bearer_token(&req), Some("abc123"));
    }

    #[test]
    fn rejects_a_missing_header() {
        let req = request_with_header(None);
        assert_eq!(bearer_token(&req), None);
    }

    #[test]
    fn rejects_an_empty_bearer_value() {
        let req = request_with_header(Some("Bearer "));
        assert_eq!(bearer_token(&req), None);
    }

    #[test]
    fn rejects_a_non_bearer_scheme() {
        let req = request_with_header(Some("Basic abc123"));
        assert_eq!(bearer_token(&req), None);
    }
}
