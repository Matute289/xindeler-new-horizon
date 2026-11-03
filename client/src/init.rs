//! Code and utils for Initializing the connection to the server.
use std::{future::Future, sync::Arc};

use common_net::msg::ServerInfo;
use network::{ConnectAddr, Network};
use rustls::client::danger::ServerCertVerified;
use tracing::warn;

use crate::{
    Error, addr,
    oauth::{OAuthFailure, OAuthLogin},
};

pub enum Credentials {
    UsernamePassword(String, String),
    Username(String),
    /// Xindeler: native OAuth (Discord/Google, ...) against the server's
    /// trusted auth provider (`auth.xindeler.com`). Carries the one-shot UI
    /// callbacks the blocking OAuth flow needs, which is why `Credentials`
    /// is not `Clone` here (upstream derives it) -- see
    /// [`Credentials::try_clone`].
    OAuth(OAuthLogin),
}

impl Credentials {
    /// Clones every variant that can be cloned. `OAuth` owns boxed one-shot
    /// UI callbacks and returns `None`, which is why
    /// [`StaticCredentialProvider`] cannot hand out OAuth credentials.
    pub fn try_clone(&self) -> Option<Self> {
        match self {
            Self::UsernamePassword(username, password) => {
                Some(Self::UsernamePassword(username.clone(), password.clone()))
            },
            Self::Username(username) => Some(Self::Username(username.clone())),
            Self::OAuth(_) => None,
        }
    }
}

impl std::fmt::Debug for Credentials {
    // Hand-written so a password never ends up in a log line.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::UsernamePassword(username, _) => f
                .debug_tuple("UsernamePassword")
                .field(username)
                .field(&"<redacted>")
                .finish(),
            Self::Username(username) => f.debug_tuple("Username").field(username).finish(),
            Self::OAuth(login) => f.debug_tuple("OAuth").field(&login.provider).finish(),
        }
    }
}

pub trait CredentialProvider {
    ///provides a quick check to verify if a Auth Server is expected and
    /// trusted This function is expected to return quickly.
    fn auth_trusted(&self, auth_provider: &Option<String>) -> bool;

    ///Called by the client once we are connected to the server and the server
    /// is requiring us to provide Credentials.
    /// This function is expected to trigger UI to wait for a result. So the
    /// User might take a minute or so to log in.
    /// Throw Error in case you want to abort login.
    fn get_credentials(
        &self,
        auth_provider: &Option<String>,
    ) -> impl Future<Output = Result<Credentials, ()>> + Send;

    /// Xindeler: called when the auth server answered a correct password (or
    /// a finished OAuth login) with a 2FA (TOTP) challenge. Return the code
    /// the player typed, or `None` to cancel the login. The challenge is
    /// redeemed right after, through `POST {auth_provider}/login/2fa`.
    ///
    /// Defaults to `None`: providers without a player to ask (bots,
    /// [`StaticCredentialProvider`]) simply fail the login on a 2FA account.
    fn get_two_fa_code(
        &self,
        auth_provider: &Option<String>,
    ) -> impl Future<Output = Option<String>> + Send {
        let _ = auth_provider;
        std::future::ready(None)
    }
}

/// Use in case you already know your Auth provider Username and Password
/// upfront.
#[derive(Default)]
pub struct StaticCredentialProvider {
    pub credentials_by_auth: std::collections::HashMap<Option<String>, Credentials>,
}

impl CredentialProvider for StaticCredentialProvider {
    fn auth_trusted(&self, auth_provider: &Option<String>) -> bool {
        self.credentials_by_auth.contains_key(auth_provider)
    }

    async fn get_credentials(&self, auth_provider: &Option<String>) -> Result<Credentials, ()> {
        self.credentials_by_auth
            .get(auth_provider)
            .and_then(Credentials::try_clone)
            .ok_or(())
    }
}

#[derive(Debug)]
pub enum ClientInitStage {
    /// A connection to the server is being created
    ConnectionEstablish,
    /// Waiting for server version
    WatingForServerVersion,
    /// Server Version successfuly arrived, do a first PING
    ServerVersionArrived(ServerInfo),
    /// We're currently authenticating with the server
    Authentication,
    /// Loading map data, site information, recipe information and other
    /// initialization data
    LoadingInitData,
    /// Prepare data received by the server to be used by the client (insert
    /// data into the ECS, render map)
    StartingClient,
    /// Requesting Plugins from Server and downloading them
    RequestingPlugins,
}

/// Xindeler: what `Client::register` has to do to turn the provider's
/// [`Credentials`] into the server's `token_or_username`.
///
/// Upstream does this in one async `resolve_token_or_username` on top of the
/// async GitLab `authc`. Ours is split in two because `xindeler-authc` is a
/// blocking API that must run inside `spawn_blocking` (see
/// `Client::acquire_auth_token`), and because 2FA has to go back to the
/// [`CredentialProvider`] between two blocking calls: this pure part picks
/// the flow, `Client::register` runs it.
pub(crate) enum AuthFlow {
    /// Send the username as-is (no auth server, or a username-only
    /// credential -- same as upstream).
    PlainUsername(String),
    /// Password sign-in against the auth server, possibly followed by 2FA.
    Password {
        auth_addr: String,
        username: String,
        password: String,
    },
    /// Native OAuth against the auth server, possibly followed by 2FA.
    OAuth {
        auth_addr: String,
        login: OAuthLogin,
    },
}

/// Picks the [`AuthFlow`] for `credentials` against the server's advertised
/// auth provider.
/// NOTE: only call it if you trust the auth_provider!
pub(crate) fn select_auth_flow(
    credentials: Credentials,
    trusted_auth_provider: &Option<String>,
) -> Result<AuthFlow, Error> {
    match (credentials, trusted_auth_provider) {
        (Credentials::Username(username), _)
        | (Credentials::UsernamePassword(username, _), None) => {
            Ok(AuthFlow::PlainUsername(username))
        },
        (Credentials::UsernamePassword(username, password), Some(addr)) => Ok(AuthFlow::Password {
            auth_addr: addr.clone(),
            username,
            password,
        }),
        (Credentials::OAuth(login), Some(addr)) => Ok(AuthFlow::OAuth {
            auth_addr: addr.clone(),
            login,
        }),
        // A server with no auth provider has no OAuth flow to run and no
        // account to link the provider to.
        (Credentials::OAuth(_), None) => Err(Error::OAuthFailed(OAuthFailure::NoAuthServer)),
    }
}

pub(crate) async fn connect_quic(
    network: &Network,
    hostname: String,
    override_port: Option<u16>,
    prefer_ipv6: bool,
    validate_tls: bool,
) -> Result<network::Participant, crate::error::Error> {
    let config = if validate_tls {
        quinn::ClientConfig::try_with_platform_verifier()?
    } else {
        warn!(
            "skipping validation of server identity. There is no guarantee that the server you're \
             connected to is the one you expect to be connecting to."
        );
        #[derive(Debug)]
        struct Verifier;
        impl rustls::client::danger::ServerCertVerifier for Verifier {
            fn verify_server_cert(
                &self,
                _end_entity: &rustls::pki_types::CertificateDer<'_>,
                _intermediates: &[rustls::pki_types::CertificateDer<'_>],
                _server_name: &rustls::pki_types::ServerName<'_>,
                _ocsp_response: &[u8],
                _now: rustls::pki_types::UnixTime,
            ) -> Result<ServerCertVerified, rustls::Error> {
                Ok(ServerCertVerified::assertion())
            }

            fn verify_tls12_signature(
                &self,
                _message: &[u8],
                _cert: &rustls::pki_types::CertificateDer<'_>,
                _dss: &rustls::DigitallySignedStruct,
            ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error>
            {
                Ok(rustls::client::danger::HandshakeSignatureValid::assertion())
            }

            fn verify_tls13_signature(
                &self,
                _message: &[u8],
                _cert: &rustls::pki_types::CertificateDer<'_>,
                _dss: &rustls::DigitallySignedStruct,
            ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error>
            {
                Ok(rustls::client::danger::HandshakeSignatureValid::assertion())
            }

            fn supported_verify_schemes(&self) -> Vec<rustls::SignatureScheme> {
                vec![
                    rustls::SignatureScheme::RSA_PKCS1_SHA1,
                    rustls::SignatureScheme::ECDSA_SHA1_Legacy,
                    rustls::SignatureScheme::RSA_PKCS1_SHA256,
                    rustls::SignatureScheme::ECDSA_NISTP256_SHA256,
                    rustls::SignatureScheme::RSA_PKCS1_SHA384,
                    rustls::SignatureScheme::ECDSA_NISTP384_SHA384,
                    rustls::SignatureScheme::RSA_PKCS1_SHA512,
                    rustls::SignatureScheme::ECDSA_NISTP521_SHA512,
                    rustls::SignatureScheme::RSA_PSS_SHA256,
                    rustls::SignatureScheme::RSA_PSS_SHA384,
                    rustls::SignatureScheme::RSA_PSS_SHA512,
                    rustls::SignatureScheme::ED25519,
                    rustls::SignatureScheme::ED448,
                ]
            }
        }

        let mut cfg = rustls::ClientConfig::builder()
            .dangerous()
            .with_custom_certificate_verifier(Arc::new(Verifier))
            .with_no_client_auth();
        cfg.enable_early_data = true;

        quinn::ClientConfig::new(Arc::new(
            quinn::crypto::rustls::QuicClientConfig::try_from(cfg).unwrap(),
        ))
    };

    addr::try_connect(network, &hostname, override_port, prefer_ipv6, |a| {
        ConnectAddr::Quic(a, config.clone(), hostname.clone())
    })
    .await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::oauth::OAuthProvider;

    const AUTH: &str = "https://auth.xindeler.com";

    fn oauth_login() -> OAuthLogin {
        OAuthLogin {
            provider: OAuthProvider::Discord,
            cancelled: Box::new(|| false),
            on_browser_opened: Box::new(|| {}),
            pick_username: Box::new(|_| None),
        }
    }

    fn password() -> Credentials {
        Credentials::UsernamePassword("alice".to_owned(), "hunter2".to_owned())
    }

    fn block_on<F: Future>(fut: F) -> F::Output {
        tokio::runtime::Builder::new_current_thread()
            .build()
            .expect("test runtime")
            .block_on(fut)
    }

    #[test]
    fn password_against_an_auth_server_signs_in() {
        let flow = select_auth_flow(password(), &Some(AUTH.to_owned()));
        assert!(matches!(
            flow,
            Ok(AuthFlow::Password { ref auth_addr, ref username, ref password })
                if auth_addr == AUTH && username == "alice" && password == "hunter2"
        ));
    }

    #[test]
    fn password_without_an_auth_server_sends_the_plain_username() {
        let flow = select_auth_flow(password(), &None);
        assert!(matches!(flow, Ok(AuthFlow::PlainUsername(ref u)) if u == "alice"));
    }

    #[test]
    fn username_only_always_sends_the_plain_username() {
        for auth in [None, Some(AUTH.to_owned())] {
            let flow = select_auth_flow(Credentials::Username("bob".to_owned()), &auth);
            assert!(matches!(flow, Ok(AuthFlow::PlainUsername(ref u)) if u == "bob"));
        }
    }

    #[test]
    fn oauth_against_an_auth_server_runs_the_oauth_flow() {
        let flow = select_auth_flow(Credentials::OAuth(oauth_login()), &Some(AUTH.to_owned()));
        assert!(matches!(
            flow,
            Ok(AuthFlow::OAuth { ref auth_addr, ref login })
                if auth_addr == AUTH && login.provider == OAuthProvider::Discord
        ));
    }

    #[test]
    fn oauth_without_an_auth_server_fails_with_no_auth_server() {
        let flow = select_auth_flow(Credentials::OAuth(oauth_login()), &None);
        assert!(matches!(
            flow,
            Err(Error::OAuthFailed(OAuthFailure::NoAuthServer))
        ));
    }

    #[test]
    fn static_provider_trusts_and_answers_only_its_own_auth_servers() {
        let provider = StaticCredentialProvider {
            credentials_by_auth: [(Some(AUTH.to_owned()), password())].into(),
        };
        let other = Some("https://evil.example".to_owned());

        assert!(provider.auth_trusted(&Some(AUTH.to_owned())));
        assert!(!provider.auth_trusted(&other));
        assert!(!provider.auth_trusted(&None));

        let creds = block_on(provider.get_credentials(&Some(AUTH.to_owned())));
        assert!(matches!(
            creds,
            Ok(Credentials::UsernamePassword(ref u, ref p)) if u == "alice" && p == "hunter2"
        ));
        assert!(block_on(provider.get_credentials(&other)).is_err());
    }

    #[test]
    fn static_provider_never_hands_out_oauth_and_never_answers_2fa() {
        let provider = StaticCredentialProvider {
            credentials_by_auth: [(Some(AUTH.to_owned()), Credentials::OAuth(oauth_login()))]
                .into(),
        };
        assert!(block_on(provider.get_credentials(&Some(AUTH.to_owned()))).is_err());
        assert_eq!(
            block_on(provider.get_two_fa_code(&Some(AUTH.to_owned()))),
            None
        );
    }

    #[test]
    fn credentials_debug_never_prints_the_password() {
        let printed = format!("{:?}", password());
        assert!(printed.contains("alice"), "{printed}");
        assert!(!printed.contains("hunter2"), "{printed}");
    }
}
