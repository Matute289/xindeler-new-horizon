pub mod loopback;

pub mod api;

use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use rand::Rng;
use sha2::{Digest, Sha256};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OAuthProvider {
    Discord,
    Google,
}

impl OAuthProvider {
    pub fn slug(self) -> &'static str {
        match self {
            OAuthProvider::Discord => "discord",
            OAuthProvider::Google => "google",
        }
    }
}

#[derive(Debug)]
pub enum OAuthFailure {
    /// The game server advertises no auth provider, so there is nothing to
    /// sign in against.
    NoAuthServer,
    /// `127.0.0.1:0` could not be bound (firewall, sandbox, hardened desktop),
    /// so the browser has nowhere to deliver the pickup code. Loopback is the
    /// only delivery path (2026-08-21 erratum), so this ends the attempt
    /// immediately -- there is nothing to fall back to. Carries the io error's
    /// message for the log line, never shown verbatim to the player.
    ListenerBindFailed(String),
    /// The auth server returned an `authorize_url` that is not https, or
    /// whose host is not one of the small fixed set of real OAuth provider
    /// hosts (see `TRUSTED_OAUTH_PROVIDER_HOSTS`) -- notably, this rejects
    /// the auth server's own host too; the authorize URL must always point
    /// directly at the provider, never back at the auth server. Never opened.
    UntrustedAuthorizeUrl(String),
    /// The 5-minute attempt window elapsed.
    Timeout,
    /// The player cancelled. Never displayed -- cancelling drops the whole
    /// `ClientInit`, the same way `CancelConnect` already does.
    Cancelled,
    /// Transport failure, malformed response, or a server error body.
    Other(String),
}

/// PKCE-style pair. `verifier` never leaves this process until the
/// server-to-server exchange call; that is the entire defense against another
/// local process squatting the loopback port (spec §5).
pub struct Pkce {
    pub verifier: String,
    pub challenge: String,
}

impl Pkce {
    pub fn generate() -> Self {
        let mut bytes = [0u8; 32];
        rand::rng().fill_bytes(&mut bytes);
        let verifier = URL_SAFE_NO_PAD.encode(bytes);
        let challenge = URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()));
        Self {
            verifier,
            challenge,
        }
    }
}

/// The two real OAuth provider authorize hosts, mirroring
/// `DiscordProvider`/`GoogleProvider::authorize_url()` in `xindeler-auth`'s
/// `server/src/oauth.rs` verbatim. Fixed, not configurable -- unlike the auth
/// server's own address, these are never something a player or a `settings.ron`
/// entry should be able to widen.
const TRUSTED_OAUTH_PROVIDER_HOSTS: &[&str] = &["discord.com", "accounts.google.com"];

/// The `authorize_url` comes from `/oauth/{provider}/start`'s redirect
/// `Location` header (OAUTHN-C-5's `start()`), not a constant, so it is never
/// handed to the system browser unchecked (spec S5). It always points
/// directly at the OAuth provider, never at the auth server -- if `start()`'s
/// redirect target does NOT match this allowlist, that means `/start` itself
/// failed server-side and redirected to the web frontend's own error page
/// instead (xindeler-auth's `oauth_start` falls back to `oauth_error_redirect`
/// on validation failure even for native callers) -- this function rejects
/// that case too, surfacing it as a generic start failure rather than opening
/// a web page in the player's browser.
pub fn validate_authorize_url(authorize_url: &str) -> Result<(), OAuthFailure> {
    let reject = || OAuthFailure::UntrustedAuthorizeUrl(authorize_url.to_owned());

    let url = reqwest::Url::parse(authorize_url).map_err(|_| reject())?;
    if url.scheme() != "https" {
        return Err(reject());
    }
    let host = url.host_str().ok_or_else(reject)?;
    if !TRUSTED_OAUTH_PROVIDER_HOSTS.contains(&host) {
        return Err(reject());
    }

    Ok(())
}

/// The password-login path gets this same rule for free through
/// `authc::AuthClient::new`, which refuses to build a client for a plain
/// `http://` provider address unless the host is loopback (see `authc`'s
/// `is_loopback_url`/`AuthClientError::InsecureUrl`). The OAuth path never
/// constructs an `AuthClient` -- it talks to `auth_addr` directly via
/// `client/src/oauth/api.rs` -- so without this check a server advertising a
/// non-loopback `http://` `auth_provider` would send the whole OAuth
/// exchange, including the final `AuthToken` and any TOTP code, in
/// cleartext. Mirrors `authc`'s rule exactly: `https://` on any host, or
/// `http://` only for `127.0.0.1`, `::1`, or `localhost`.
fn validate_auth_addr(auth_addr: &str) -> Result<(), OAuthFailure> {
    let fail = || {
        OAuthFailure::Other(format!(
            "refusing to run the OAuth exchange over plain http against a non-loopback auth \
             server ({auth_addr})"
        ))
    };

    let url = reqwest::Url::parse(auth_addr).map_err(|_| fail())?;
    if url.scheme() == "https" {
        return Ok(());
    }
    let is_loopback = url.host_str().is_some_and(|host| {
        // `Url::host_str` brackets IPv6 addresses (e.g. `[::1]`), which
        // `IpAddr::from_str` rejects -- strip them before parsing so IPv6
        // loopback addresses are recognized correctly too.
        let host = host
            .strip_prefix('[')
            .and_then(|h| h.strip_suffix(']'))
            .unwrap_or(host);
        host.eq_ignore_ascii_case("localhost")
            || host
                .parse::<std::net::IpAddr>()
                .is_ok_and(|address| address.is_loopback())
    });
    if is_loopback { Ok(()) } else { Err(fail()) }
}

use loopback::{LoopbackListener, bind_failure};
use std::time::{Duration, Instant};

/// Overall attempt window (spec §3.3), matching the bumped native-origin
/// `OAuthStateCache` TTL on the server side (spec §4.6).
pub const ATTEMPT_TIMEOUT: Duration = Duration::from_secs(300);

/// The callbacks a native OAuth attempt needs from the frontend. Boxed rather
/// than generic so `Client::new` gains one plain parameter instead of two more
/// type parameters. Built from crossbeam channel clones on the voxygen side,
/// the same way the existing password-login `code_fn` is.
pub struct OAuthLogin {
    pub provider: OAuthProvider,
    /// Reads `ClientInit`'s existing `cancel: Arc<AtomicBool>`, which its
    /// `Drop` impl sets -- so cancel stays cancel-by-drop, with no second
    /// teardown path.
    pub cancelled: Box<dyn Fn() -> bool + Send + Sync + 'static>,
    /// Called once the system browser has actually been handed the authorize
    /// URL, so the frontend can switch to its "finish in your browser" state.
    pub on_browser_opened: Box<dyn FnMut() + Send + 'static>,
    /// Given the server's suggested username, returns the player's choice, or
    /// `None` if the prompt was cancelled.
    pub pick_username: Box<dyn FnMut(String) -> Option<String> + Send + 'static>,
}

pub enum OAuthOutcome {
    Token(authc::AuthToken),
    /// The linked account has TOTP enabled. Redeemed by the caller through the
    /// already-shipped `Client::submit_2fa_code`, so OAuth-then-2FA reuses
    /// the existing 2FA network and UI code unchanged.
    TotpRequired(serde_json::Value),
}

/// Runs one whole native OAuth attempt. Blocking throughout -- the caller must
/// already be inside a blocking context, the same way the existing
/// password-based token acquisition is.
pub fn run_oauth_login(
    auth_addr: &str,
    login: &mut OAuthLogin,
) -> Result<OAuthOutcome, OAuthFailure> {
    validate_auth_addr(auth_addr)?;

    let deadline = Instant::now() + ATTEMPT_TIMEOUT;
    let pkce = Pkce::generate();
    let http = api::http_client()?;

    // Bind before anything is sent: the port is part of the `/start` request,
    // and a bind failure is the end of the attempt (2026-08-21 erratum), so
    // failing here means no state was ever parked server-side and no browser
    // window was ever opened.
    let listener = LoopbackListener::bind().map_err(bind_failure)?;
    let port = listener.port();

    let started = api::start(&http, auth_addr, login.provider, &pkce.challenge, port)?;
    validate_authorize_url(&started.authorize_url)?;

    open::that_detached(&started.authorize_url).map_err(|e| OAuthFailure::Other(e.to_string()))?;
    (login.on_browser_opened)();

    let pickup = listener.wait_for_pickup(deadline, login.cancelled.as_ref())?;

    match api::exchange(&http, auth_addr, &pickup, &pkce.verifier)? {
        api::ExchangeResponse::SignedIn { token } => Ok(OAuthOutcome::Token(token)),
        api::ExchangeResponse::TotpRequired { challenge_id } => {
            Ok(OAuthOutcome::TotpRequired(challenge_id))
        },
        api::ExchangeResponse::PendingRegistration {
            pending_token,
            suggested_username,
        } => {
            let Some(username) = (login.pick_username)(suggested_username) else {
                return Err(OAuthFailure::Cancelled);
            };
            api::complete_registration(&http, auth_addr, &pending_token, &username)
                .map(OAuthOutcome::Token)
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn provider_slugs_are_the_url_path_segments() {
        assert_eq!(OAuthProvider::Discord.slug(), "discord");
        assert_eq!(OAuthProvider::Google.slug(), "google");
    }

    #[test]
    fn pkce_verifier_is_43_base64url_chars_of_32_random_bytes() {
        let pkce = Pkce::generate();
        assert_eq!(pkce.verifier.len(), 43);
        assert!(
            pkce.verifier
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
        );
    }

    #[test]
    fn pkce_challenge_is_the_base64url_sha256_of_the_verifier() {
        let pkce = Pkce::generate();
        let expected = URL_SAFE_NO_PAD.encode(Sha256::digest(pkce.verifier.as_bytes()));
        assert_eq!(pkce.challenge, expected);
        assert_eq!(pkce.challenge.len(), 43);
    }

    #[test]
    fn two_generated_pkce_pairs_differ() {
        assert_ne!(Pkce::generate().verifier, Pkce::generate().verifier);
    }

    #[test]
    fn accepts_the_real_discord_authorize_host() {
        assert!(
            validate_authorize_url("https://discord.com/oauth2/authorize?client_id=1&state=x")
                .is_ok()
        );
    }

    #[test]
    fn accepts_the_real_google_authorize_host() {
        assert!(
            validate_authorize_url(
                "https://accounts.google.com/o/oauth2/v2/auth?client_id=1&state=x"
            )
            .is_ok()
        );
    }

    #[test]
    fn rejects_a_different_host() {
        let err = validate_authorize_url("https://evil.example/steal?state=x")
            .expect_err("different host must be rejected");
        assert!(matches!(err, OAuthFailure::UntrustedAuthorizeUrl(_)));
    }

    #[test]
    fn rejects_the_auth_servers_own_host() {
        // authorize_url never points at auth.xindeler.com itself -- if it
        // does, /start's redirect was an error page, not a real authorize
        // URL (spec S3.2 step 4's note on how start() surfaces failures).
        assert!(
            validate_authorize_url("https://auth.xindeler.com/oauth/callback#error=x").is_err()
        );
    }

    #[test]
    fn rejects_a_lookalike_subdomain() {
        assert!(validate_authorize_url("https://discord.com.evil.example/x").is_err());
        assert!(validate_authorize_url("https://evil.discord.com/x").is_err());
        assert!(validate_authorize_url("https://accounts.google.com.evil.example/x").is_err());
    }

    #[test]
    fn rejects_plain_http_even_on_a_real_provider_host() {
        assert!(validate_authorize_url("http://discord.com/oauth2/authorize").is_err());
    }

    #[test]
    fn rejects_non_http_schemes_and_garbage() {
        assert!(validate_authorize_url("javascript:alert(1)").is_err());
        assert!(validate_authorize_url("file:///etc/passwd").is_err());
        assert!(validate_authorize_url("not a url at all").is_err());
    }

    #[test]
    fn attempt_timeout_matches_the_five_minute_spec_value() {
        assert_eq!(ATTEMPT_TIMEOUT, std::time::Duration::from_secs(300));
    }

    #[test]
    fn validate_auth_addr_accepts_https_on_any_host() {
        assert!(validate_auth_addr("https://auth.xindeler.com").is_ok());
        assert!(validate_auth_addr("https://example.com").is_ok());
    }

    #[test]
    fn validate_auth_addr_accepts_http_on_loopback_variants() {
        assert!(validate_auth_addr("http://127.0.0.1:19253").is_ok());
        assert!(validate_auth_addr("http://localhost:19253").is_ok());
        assert!(validate_auth_addr("http://[::1]:19253").is_ok());
        // Host matching is case-insensitive, same as `authc`'s rule.
        assert!(validate_auth_addr("http://LOCALHOST:19253").is_ok());
    }

    #[test]
    fn validate_auth_addr_rejects_http_on_a_real_host() {
        let err = validate_auth_addr("http://auth.xindeler.com")
            .expect_err("plain http to a non-loopback host must be rejected");
        assert!(matches!(err, OAuthFailure::Other(_)));
    }

    #[test]
    fn validate_auth_addr_rejects_garbage() {
        assert!(validate_auth_addr("not a url at all").is_err());
    }
}
