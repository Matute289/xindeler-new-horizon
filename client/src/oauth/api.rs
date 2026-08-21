use crate::oauth::{OAuthFailure, OAuthProvider};
use reqwest::blocking::Client;
use std::time::Duration;

pub struct StartResponse {
    pub authorize_url: String,
    pub state: String,
}

#[derive(serde::Deserialize)]
pub struct TokenResponse {
    pub token: authc::AuthToken,
}

#[derive(serde::Deserialize)]
struct ErrorResponse {
    code: String,
}

/// The three success shapes `/oauth/native/exchange` can return (Server
/// Contract §3). Untagged and ordered most-specific-first: a `{token}` body
/// cannot match either of the others.
///
/// `challenge_id` is an opaque `serde_json::Value` on purpose -- `authc` does
/// not export its `ChallengeId` type (see `Client::submit_2fa_code`'s doc
/// comment), and this client only ever forwards the value verbatim.
#[derive(serde::Deserialize)]
#[serde(untagged)]
pub enum ExchangeResponse {
    SignedIn {
        token: authc::AuthToken,
    },
    TotpRequired {
        challenge_id: serde_json::Value,
    },
    PendingRegistration {
        pending_token: String,
        suggested_username: String,
    },
}

/// Same timeouts `submit_2fa_code` already uses -- these are single
/// round-trips to the same auth server. `redirect::Policy::none()` matches
/// `authc`'s own client (`authc/src/lib.rs:83`) and is required here: `start()`
/// needs the raw `302` and its `Location` header, not an auto-followed
/// response from Discord/Google's own authorize page. `exchange` and
/// `complete_registration` never redirect in practice, so sharing one client
/// with this policy costs them nothing.
pub fn http_client() -> Result<Client, OAuthFailure> {
    Client::builder()
        .timeout(Duration::from_secs(5))
        .connect_timeout(Duration::from_secs(2))
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .map_err(|e| OAuthFailure::Other(e.to_string()))
}

fn start_url(auth_addr: &str, provider: OAuthProvider, code_challenge: &str, port: u16) -> String {
    let base = auth_addr.trim_end_matches('/');
    let slug = provider.slug();
    format!(
        "{base}/oauth/{slug}/start?client=native&code_challenge={code_challenge}&\
         redirect_port={port}"
    )
}

fn error_from_body(body: &str) -> String {
    serde_json::from_str::<ErrorResponse>(body)
        .map(|e| e.code)
        .unwrap_or_else(|_| "unparseable error response".to_owned())
}

fn transport(e: reqwest::Error) -> OAuthFailure { OAuthFailure::Other(e.to_string()) }

/// `/oauth/{provider}/start` is a plain `302` for native callers too (spec
/// S4.2's correction -- it does NOT grow a `200 JSON` contract). The
/// `Location` header's value IS the `authorize_url`; its `state` query param
/// is kept here as a structural check that the redirect really is a provider
/// authorize URL rather than the web frontend's error page. The native client
/// never sends `state` anywhere itself -- with polling delivery gone
/// (2026-08-21 erratum) the result is keyed by the pickup code the browser
/// hands to the loopback listener. Extraction matches how `oauth_start`'s own
/// server-side test reads `state` from the same header (`server/src/web.rs`'s
/// `native_start`-style tests, in the canonical xindeler-auth-side plan).
fn parse_start_location(location: &str) -> Result<StartResponse, OAuthFailure> {
    let fail = || OAuthFailure::Other("malformed /start redirect".to_owned());
    let url = reqwest::Url::parse(location).map_err(|_| fail())?;
    let state = url
        .query_pairs()
        .find(|(k, _)| k == "state")
        .map(|(_, v)| v.into_owned())
        .ok_or_else(fail)?;
    Ok(StartResponse {
        authorize_url: location.to_owned(),
        state,
    })
}

/// `port` is required, never optional: the loopback listener is already bound
/// by the time this is called, or the attempt never got here (2026-08-21
/// erratum -- there is no delivery mode without a bound port to fall back to).
pub fn start(
    http: &Client,
    auth_addr: &str,
    provider: OAuthProvider,
    code_challenge: &str,
    port: u16,
) -> Result<StartResponse, OAuthFailure> {
    let resp = http
        .get(start_url(auth_addr, provider, code_challenge, port))
        .send()
        .map_err(transport)?;
    if !resp.status().is_redirection() {
        let body = resp.text().unwrap_or_default();
        return Err(OAuthFailure::Other(error_from_body(&body)));
    }
    let location = resp
        .headers()
        .get(reqwest::header::LOCATION)
        .and_then(|v| v.to_str().ok())
        .ok_or_else(|| OAuthFailure::Other("missing Location header on /start".to_owned()))?
        .to_owned();
    parse_start_location(&location)
}

pub fn exchange(
    http: &Client,
    auth_addr: &str,
    pickup_code: &str,
    code_verifier: &str,
) -> Result<ExchangeResponse, OAuthFailure> {
    let url = format!("{}/oauth/native/exchange", auth_addr.trim_end_matches('/'));
    let resp = http
        .post(url)
        .json(&serde_json::json!({
            "pickup_code": pickup_code,
            "code_verifier": code_verifier,
        }))
        .send()
        .map_err(transport)?;
    if !resp.status().is_success() {
        let body = resp.text().unwrap_or_default();
        return Err(OAuthFailure::Other(error_from_body(&body)));
    }
    resp.json::<ExchangeResponse>().map_err(transport)
}

/// Reuses the EXISTING `/oauth/confirm-registration` endpoint
/// (`xindeler-auth/server/src/web.rs:373-395`) -- already JSON
/// `{pending_token, username}` -> `SignInResponse{token}`, so it works for the
/// native client unchanged. No `/oauth/native/complete` route exists or is
/// needed (an earlier draft of this plan assumed one; corrected 2026-08-21).
pub fn complete_registration(
    http: &Client,
    auth_addr: &str,
    pending_token: &str,
    username: &str,
) -> Result<authc::AuthToken, OAuthFailure> {
    let url = format!(
        "{}/oauth/confirm-registration",
        auth_addr.trim_end_matches('/')
    );
    let resp = http
        .post(url)
        .json(&serde_json::json!({
            "pending_token": pending_token,
            "username": username,
        }))
        .send()
        .map_err(transport)?;
    if !resp.status().is_success() {
        let body = resp.text().unwrap_or_default();
        return Err(OAuthFailure::Other(error_from_body(&body)));
    }
    resp.json::<TokenResponse>()
        .map(|body| body.token)
        .map_err(transport)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exchange_response_parses_the_direct_sign_in_shape() {
        // `authc::AuthToken` deserializes via `FromStr`, which requires
        // exactly 16 bytes of hex (32 hex chars) -- see
        // `xindeler-auth-common`'s `AuthToken` impl. The brief's original
        // 8-char fixture ("aabbccdd") doesn't satisfy that and was fixed here
        // to a valid 32-char token; flagged in the task report.
        let parsed: ExchangeResponse =
            serde_json::from_str(r#"{"token":"00112233445566778899aabbccddeeff"}"#).expect("parse");
        assert!(matches!(parsed, ExchangeResponse::SignedIn { .. }));
    }

    #[test]
    fn exchange_response_parses_the_totp_challenge_shape() {
        let parsed: ExchangeResponse =
            serde_json::from_str(r#"{"challenge_id":"ch_123"}"#).expect("parse");
        let ExchangeResponse::TotpRequired { challenge_id } = parsed else {
            panic!("expected a TOTP challenge");
        };
        assert_eq!(challenge_id, serde_json::json!("ch_123"));
    }

    #[test]
    fn exchange_response_forwards_an_opaque_non_string_challenge_id_verbatim() {
        let parsed: ExchangeResponse =
            serde_json::from_str(r#"{"challenge_id":{"id":7}}"#).expect("parse");
        let ExchangeResponse::TotpRequired { challenge_id } = parsed else {
            panic!("expected a TOTP challenge");
        };
        assert_eq!(challenge_id, serde_json::json!({"id": 7}));
    }

    #[test]
    fn exchange_response_parses_the_pending_registration_shape() {
        let parsed: ExchangeResponse =
            serde_json::from_str(r#"{"pending_token":"pt_1","suggested_username":"Matias"}"#)
                .expect("parse");
        let ExchangeResponse::PendingRegistration {
            pending_token,
            suggested_username,
        } = parsed
        else {
            panic!("expected a pending registration");
        };
        assert_eq!(pending_token, "pt_1");
        assert_eq!(suggested_username, "Matias");
    }

    #[test]
    fn start_response_extracts_state_from_the_location_query_string() {
        let parsed = parse_start_location(
            "https://discord.com/oauth2/authorize?client_id=1&state=s1&scope=identify",
        )
        .expect("parse");
        assert_eq!(parsed.state, "s1");
        assert_eq!(
            parsed.authorize_url,
            "https://discord.com/oauth2/authorize?client_id=1&state=s1&scope=identify"
        );
    }

    #[test]
    fn start_response_rejects_a_location_with_no_state_param() {
        assert!(parse_start_location("https://discord.com/oauth2/authorize?client_id=1").is_err());
    }

    #[test]
    fn start_response_rejects_an_unparseable_location() {
        assert!(parse_start_location("not a url").is_err());
    }

    #[test]
    fn start_url_carries_the_loopback_port() {
        let url = start_url(
            "https://auth.xindeler.com/",
            OAuthProvider::Discord,
            "chal",
            51234,
        );
        assert!(url.starts_with("https://auth.xindeler.com/oauth/discord/start?"));
        assert!(url.contains("client=native"));
        assert!(url.contains("code_challenge=chal"));
        assert!(url.contains("redirect_port=51234"));
    }

    #[test]
    fn error_body_is_surfaced_as_its_code() {
        assert_eq!(
            error_from_body(r#"{"code":"PICKUP_INVALID","message":"nope"}"#),
            "PICKUP_INVALID"
        );
        assert_eq!(error_from_body("not json"), "unparseable error response");
    }
}
