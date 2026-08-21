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

/// How the client expects to receive the pickup code. Chosen locally before
/// the auth server is contacted, because only the client can know whether its
/// loopback bind succeeded (spec §2.2).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OAuthDeliveryMode {
    Loopback,
    Poll,
}

#[derive(Debug)]
pub enum OAuthFailure {
    /// The game server advertises no auth provider, so there is nothing to
    /// sign in against.
    NoAuthServer,
    /// The auth server returned an `authorize_url` that is not https, or whose
    /// host is not the auth server the player already trusted. Never opened.
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
}
