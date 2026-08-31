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
    body::Body,
    extract::{Path, Request, State},
    http::{
        HeaderMap, HeaderValue,
        header::{
            AUTHORIZATION, CACHE_CONTROL, CONTENT_TYPE, ETAG, IF_NONE_MATCH, RETRY_AFTER, VARY,
        },
    },
    middleware::Next,
    response::{IntoResponse, Response},
    routing::{get, post},
};
use hyper::StatusCode;
use serde::Deserialize;
use server::authc::{AuthClient, AuthClientError, CharacterAccessToken};
use std::sync::Arc;
use tracing::error;

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

/// Every route in this family, behind the bearer check.
///
/// **The `.layer` below only wraps routes declared above it.** A route added
/// after it compiles and serves perfectly happily, with no authentication at
/// all and no `AuthenticatedUuid` extension for its handler to destructure --
/// which is a panic at request time in the best case and an unauthenticated
/// read of somebody's character data in the worst. New routes go with the
/// others, above the layer.
pub fn router(web_ui_request_s: UiRequestSender, auth_client: Option<Arc<AuthClient>>) -> Router {
    Router::new()
        .route("/characters", get(characters))
        .route("/characters/{character_id}/rename", post(rename))
        .route("/characters/{character_id}/portrait", get(portrait))
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

/// How long a caller may reuse a portrait before asking about it again.
///
/// `private` because a portrait is only ever served to the one account that
/// owns the character, so no shared cache between here and that browser may
/// keep a copy. The five minutes is a ceiling on how stale a picture can look
/// after the player re-equips something, not a correctness mechanism: an
/// `If-None-Match` revalidation after it expires is a database point read, and
/// the `ETag` is what actually decides whether anything is re-sent.
const PORTRAIT_CACHE_CONTROL: HeaderValue = HeaderValue::from_static("private, max-age=300");

/// What a cached portrait is allowed to be reused *for*.
///
/// Without this, `private, max-age=300` is a hole straight through the
/// ownership check: caches key on the URL, and the only thing distinguishing
/// two accounts asking for `/characters/42/portrait` is the `Authorization`
/// header. One account's browser profile could hand the next account's session
/// an image out of its own cache without the request ever reaching this server
/// -- five minutes wide, and invisible from here. Naming the header makes the
/// tokens part of the cache key.
const PORTRAIT_VARY: HeaderValue = HeaderValue::from_static("authorization");

/// Seconds a caller is asked to wait after being turned away because the render
/// queue was full.
///
/// Tuned for the case that actually happens rather than the worst one. A warm
/// render is on the order of ten milliseconds, so a full queue normally drains
/// in far less than a second and five is already generous. The worst case is
/// much worse -- four queued renders each allowed fifteen seconds before they
/// are killed -- and a caller that walks into *that* gets a second `503` and
/// asks again, which is the right outcome: waiting a guaranteed minute on
/// every full queue, to be correct about a case that means the renderer is
/// already broken, would make the common one feel broken too.
const PORTRAIT_RETRY_AFTER: HeaderValue = HeaderValue::from_static("5");

/// The caller's `If-None-Match`, if it sent one this layer can read.
///
/// A header that is not valid UTF-8 is treated as absent rather than as a
/// failure: the worst it costs is sending an image the caller may already have,
/// and the value is only ever compared against a digest anyway. Only the first
/// such header is read; a client that splits its tags across several loses the
/// later ones and is served the image, which is correct, if wasteful.
fn if_none_match(headers: &HeaderMap) -> Option<String> {
    headers
        .get(IF_NONE_MATCH)
        .and_then(|value| value.to_str().ok())
        .map(str::to_owned)
}

/// Turns the answer the game server sent back into the response the caller
/// gets.
///
/// Split out from [`portrait`] so the mapping can be tested without a running
/// portrait service: every case here is decided by the variant alone.
fn portrait_response(answer: MessageReturn) -> Response {
    match answer {
        MessageReturn::CharacterPortrait {
            bytes,
            format,
            etag,
        } => {
            // `format` and `etag` are both server-produced -- a media subtype
            // this server chose and a hex digest it computed -- so neither can
            // legitimately fail to be a header value. Checked rather than
            // unwrapped all the same: the alternative to a 500 here is a panic
            // inside a request handler, and these two strings arrive from a
            // database column, which is the one place a future bug could put
            // something else.
            let (Ok(content_type), Ok(etag)) = (
                HeaderValue::try_from(format!("image/{format}")),
                HeaderValue::try_from(format!("\"{etag}\"")),
            ) else {
                error!(%format, "a cached portrait describes itself in a way no header can carry");
                return StatusCode::INTERNAL_SERVER_ERROR.into_response();
            };

            (
                StatusCode::OK,
                [
                    (CONTENT_TYPE, content_type),
                    (ETAG, etag),
                    (CACHE_CONTROL, PORTRAIT_CACHE_CONTROL),
                    (VARY, PORTRAIT_VARY),
                ],
                Body::from(bytes),
            )
                .into_response()
        },
        MessageReturn::CharacterPortraitNotModified { etag } => {
            let Ok(etag) = HeaderValue::try_from(format!("\"{etag}\"")) else {
                error!("a portrait tag cannot be carried in a header");
                return StatusCode::INTERNAL_SERVER_ERROR.into_response();
            };
            // No body, by definition of the status. The caching headers are
            // repeated rather than left off: a `304` is allowed to update what
            // the cache already holds, so sending them makes the refreshed
            // entry's rules identical to the ones it was stored under instead
            // of leaving that to each cache's own defaults.
            (StatusCode::NOT_MODIFIED, [
                (ETAG, etag),
                (CACHE_CONTROL, PORTRAIT_CACHE_CONTROL),
                (VARY, PORTRAIT_VARY),
            ])
                .into_response()
        },
        MessageReturn::CharacterPortraitBusy => {
            let headers = [(RETRY_AFTER, PORTRAIT_RETRY_AFTER)];
            (StatusCode::SERVICE_UNAVAILABLE, headers).into_response()
        },
        // Not found and not yours are the same answer on purpose: anything
        // that distinguished them would let a caller walk the character-id
        // space and learn which ids exist.
        MessageReturn::CharacterPortraitNotFound => StatusCode::NOT_FOUND.into_response(),
        // The reason is already logged, with the character id, by whichever
        // step produced it, and none of it goes to the caller -- unlike
        // `rename`, where the refusal reason is something the player asked for
        // and can act on, a failed render is this server's problem alone.
        MessageReturn::CharacterPortraitFailed => StatusCode::INTERNAL_SERVER_ERROR.into_response(),
        // An answer to somebody else's message: a bug on this side, and not
        // one to describe to a caller either.
        _ => StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    }
}

/// Serves the caller's own character's portrait.
///
/// Nothing here decides anything about the image: ownership, staleness,
/// caching and rendering all happen on the game server's portrait worker, and
/// this handler forwards a request and dresses the answer in headers. The one
/// piece of caller-supplied data it reads beyond the path id is
/// `If-None-Match`, which it passes through untouched.
async fn portrait(
    State(web_ui_request_s): State<UiRequestSender>,
    Extension(AuthenticatedUuid(uuid)): Extension<AuthenticatedUuid>,
    Path(character_id): Path<i64>,
    headers: HeaderMap,
) -> Response {
    let (sender, receiver) = tokio::sync::oneshot::channel();
    let _ = web_ui_request_s
        .send((
            Message::GetCharacterPortrait {
                // The redeemed token's account, never the path. Ownership of
                // `character_id` is checked against this and nothing else.
                uuid,
                character_id,
                if_none_match: if_none_match(&headers),
            },
            sender,
        ))
        .await;
    match receiver.await {
        Ok(answer) => portrait_response(answer),
        // The game server dropped the request without answering -- it shut
        // down mid-request, or the queue was closed.
        Err(_) => StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    }
}

#[cfg(test)]
mod tests {
    use super::{bearer_token, if_none_match, portrait_response};
    use crate::cli::MessageReturn;
    use axum::{
        extract::Request,
        http::{
            HeaderMap,
            header::{CACHE_CONTROL, CONTENT_TYPE, ETAG, IF_NONE_MATCH, RETRY_AFTER, VARY},
        },
        response::Response,
    };
    use hyper::StatusCode;

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

    const ETAG_VALUE: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

    fn headers(name: axum::http::HeaderName, value: &[u8]) -> HeaderMap {
        let mut headers = HeaderMap::new();
        headers.insert(name, axum::http::HeaderValue::from_bytes(value).unwrap());
        headers
    }

    /// The mapping under test builds bodies lazily, so reading one back needs
    /// somewhere to poll it. A current-thread runtime built per call is ample
    /// for bodies that are already entirely in memory.
    fn body_bytes(response: Response) -> Vec<u8> {
        tokio::runtime::Builder::new_current_thread()
            .build()
            .unwrap()
            .block_on(async {
                use http_body_util::BodyExt;
                response.into_body().collect().await.unwrap().to_bytes()
            })
            .to_vec()
    }

    #[test]
    fn reads_an_if_none_match_header() {
        assert_eq!(
            if_none_match(&headers(IF_NONE_MATCH, b"\"abc\"")),
            Some("\"abc\"".to_owned())
        );
    }

    #[test]
    fn treats_a_missing_if_none_match_as_absent() {
        assert_eq!(if_none_match(&HeaderMap::new()), None);
    }

    /// A header this layer cannot read is not an error: the request is simply
    /// served the image, which is always a correct answer to a conditional
    /// request.
    #[test]
    fn treats_an_unreadable_if_none_match_as_absent() {
        assert_eq!(if_none_match(&headers(IF_NONE_MATCH, b"\xff\xfe")), None);
    }

    #[test]
    fn an_image_is_served_with_its_type_tag_and_caching_rules() {
        let response = portrait_response(MessageReturn::CharacterPortrait {
            bytes: b"IMAGE".to_vec(),
            format: "webp".to_owned(),
            etag: ETAG_VALUE.to_owned(),
        });

        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(response.headers()[CONTENT_TYPE], "image/webp");
        assert_eq!(
            response.headers()[ETAG],
            format!("\"{ETAG_VALUE}\""),
            "the tag must be quoted, which the game server deliberately does not do"
        );
        assert_eq!(response.headers()[CACHE_CONTROL], "private, max-age=300");
        assert_eq!(
            response.headers()[VARY],
            "authorization",
            "without this, a private cache may hand one account's portrait to the next session in \
             the same browser, never reaching the ownership check"
        );
        assert_eq!(body_bytes(response), b"IMAGE");
    }

    /// The media type is assembled from a database column. If a bug ever put
    /// something header-hostile in it, the response must fail rather than
    /// carry it -- a value with a newline in it is how a header becomes two.
    #[test]
    fn a_format_that_cannot_be_a_header_fails_rather_than_being_sent() {
        let response = portrait_response(MessageReturn::CharacterPortrait {
            bytes: b"IMAGE".to_vec(),
            format: "webp\r\nX-Injected: yes".to_owned(),
            etag: ETAG_VALUE.to_owned(),
        });

        assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
        assert!(response.headers().get("x-injected").is_none());
    }

    #[test]
    fn an_unchanged_portrait_is_answered_without_a_body() {
        let response = portrait_response(MessageReturn::CharacterPortraitNotModified {
            etag: ETAG_VALUE.to_owned(),
        });

        assert_eq!(response.status(), StatusCode::NOT_MODIFIED);
        assert_eq!(response.headers()[ETAG], format!("\"{ETAG_VALUE}\""));
        assert_eq!(
            response.headers()[VARY],
            "authorization",
            "a 304 updates what the cache holds, so it must not silently widen who may reuse it"
        );
        assert_eq!(response.headers()[CACHE_CONTROL], "private, max-age=300");
        assert!(body_bytes(response).is_empty());
    }

    #[test]
    fn a_full_queue_asks_the_caller_to_come_back() {
        let response = portrait_response(MessageReturn::CharacterPortraitBusy);

        assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(response.headers()[RETRY_AFTER], "5");
    }

    #[test]
    fn a_character_that_is_not_the_callers_is_not_found() {
        let response = portrait_response(MessageReturn::CharacterPortraitNotFound);

        assert_eq!(response.status(), StatusCode::NOT_FOUND);
        assert!(
            body_bytes(response).is_empty(),
            "a 404 here must say nothing at all -- anything it said would be the difference \
             between a character that does not exist and one that is somebody else's"
        );
    }

    /// Why the render failed is for this server's logs.
    #[test]
    fn a_failure_reaches_the_caller_without_its_reason() {
        let response = portrait_response(MessageReturn::CharacterPortraitFailed);

        assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
        assert!(body_bytes(response).is_empty());
    }

    /// An answer to somebody else's message can only be a bug on this side.
    /// `Error` is in the list deliberately: it is how *other* handlers report a
    /// refusal together with its reason, and that reason must not be relayed
    /// by this route even if one ever reaches it by mistake.
    #[test]
    fn an_unrelated_answer_is_a_failure_that_says_nothing() {
        for answer in [
            MessageReturn::CharacterRenamed,
            MessageReturn::Error("sqlite: disk I/O error".to_owned()),
        ] {
            let response = portrait_response(answer);

            assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
            assert!(body_bytes(response).is_empty());
        }
    }
}
