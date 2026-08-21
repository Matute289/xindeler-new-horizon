pub mod loopback;

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
}
