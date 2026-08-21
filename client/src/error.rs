use crate::oauth::OAuthFailure;
use authc::AuthClientError;
use common_net::msg::server::BanInfo;
pub use network::{InitProtocolError, NetworkConnectError, NetworkError};
use network::{ParticipantError, StreamError};
use rustls::Error as RustlsError;
use specs::error::Error as SpecsError;

/// The result of redeeming a 2FA login challenge (`POST /login/2fa` against
/// the auth server) once the player has submitted a code. Distinct from
/// `AuthClientError` -- the auth server exposes this as a `code` string in
/// its error body rather than as a status this client's transport layer
/// (`authc`) already models, so this repo maps it directly rather than
/// routing it back through that enum.
#[derive(Debug)]
pub enum TwoFaFailure {
    /// `TOTP_INVALID_CODE` -- the code was wrong.
    WrongCode,
    /// `TOTP_CHALLENGE_INVALID` -- the challenge doesn't exist or its TTL
    /// elapsed. The server does not distinguish "expired" from "unknown
    /// challenge id" any further than this.
    ChallengeExpired,
    /// `ACCOUNT_2FA_LOCKED` -- too many wrong codes across challenges.
    AccountLocked,
    /// Anything else: an unrecognized error `code`, a network failure, or a
    /// response that didn't parse. Carries a raw description for logging;
    /// the player sees the existing generic connection-error copy for this
    /// case, not the raw string.
    Other(String),
}

#[derive(Debug)]
pub enum Error {
    Kicked(String),
    NetworkErr(NetworkError),
    ParticipantErr(ParticipantError),
    StreamErr(StreamError),
    ServerTimeout,
    ServerShutdown,
    TooManyPlayers,
    NotOnWhitelist,
    AuthErr(String),
    AuthClientError(AuthClientError),
    AuthServerUrlInvalid(String),
    AuthServerNotTrusted,
    /// Redeeming a 2FA login challenge (`POST /login/2fa`) failed. See
    /// `TwoFaFailure`. Never constructed for a cancelled prompt -- that
    /// aborts the whole connect attempt the same way `CancelConnect`
    /// already does, without an error to display.
    TwoFaFailed(TwoFaFailure),
    /// Native OAuth login failed. `OAuthFailure::Cancelled`
    /// is never displayed -- cancelling aborts the whole connect attempt the
    /// same way `CancelConnect` already does.
    OAuthFailed(OAuthFailure),
    HostnameLookupFailed(std::io::Error),
    Banned(BanInfo),
    /// Persisted character data is invalid or missing
    InvalidCharacter,
    //TODO: InvalidAlias,
    Other(String),
    SpecsErr(SpecsError),
    RustlsErr(RustlsError),
}

impl From<SpecsError> for Error {
    fn from(err: SpecsError) -> Self { Self::SpecsErr(err) }
}

impl From<RustlsError> for Error {
    fn from(err: RustlsError) -> Self { Self::RustlsErr(err) }
}

impl From<NetworkError> for Error {
    fn from(err: NetworkError) -> Self { Self::NetworkErr(err) }
}

impl From<ParticipantError> for Error {
    fn from(err: ParticipantError) -> Self { Self::ParticipantErr(err) }
}

impl From<StreamError> for Error {
    fn from(err: StreamError) -> Self { Self::StreamErr(err) }
}

impl From<AuthClientError> for Error {
    fn from(err: AuthClientError) -> Self { Self::AuthClientError(err) }
}
