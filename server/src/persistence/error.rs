//! Consolidates rusqlite and validation errors under a common error type

extern crate rusqlite;

use std::fmt;

#[derive(Debug)]
pub enum PersistenceError {
    // An invalid asset was returned from the database
    AssetError(String),
    // The player has already reached the max character limit
    CharacterLimitReached,
    // An error occurred while establish a db connection
    DatabaseConnectionError(rusqlite::Error),
    // An error occurred when performing a database action
    DatabaseError(rusqlite::Error),
    // Unable to load body or stats for a character
    CharacterDataError,
    SerializationError(serde_json::Error),
    ConversionError(String),
    OtherError(String),
    // NH-79: the character doesn't exist, or exists but doesn't belong to
    // the requesting player -- deliberately the same error either way (same
    // shape `delete_character`'s ownership check already treats
    // identically, just returned as an explicit Err here instead of a
    // silent no-op, since a rename has a real client-facing outcome that
    // must not look like success).
    CharacterNotFound,
    // NH-79: `character.alias` failed `common::character::validate_character_name`.
    CharacterNameInvalid(common::character::CharacterNameError),
    // NH-79: the requested name is already taken by another character (the
    // new `UNIQUE` index on `character.alias`, migration V86).
    CharacterNameUnavailable,
}

impl fmt::Display for PersistenceError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", match self {
            Self::AssetError(error) => error.to_string(),
            Self::CharacterLimitReached => String::from("Character limit exceeded"),
            Self::DatabaseError(error) => error.to_string(),
            Self::DatabaseConnectionError(error) => error.to_string(),
            Self::CharacterDataError => String::from("Error while loading character data"),
            Self::SerializationError(error) => error.to_string(),
            Self::ConversionError(error) => error.to_string(),
            Self::OtherError(error) => error.to_string(),
            Self::CharacterNotFound => String::from("Character not found"),
            Self::CharacterNameInvalid(error) => format!("Invalid character name: {error:?}"),
            Self::CharacterNameUnavailable => String::from("Character name is already taken"),
        })
    }
}

impl PersistenceError {
    /// The message safe to hand to an external, untrusted caller (NH-79's
    /// `/player_api/v1` route family). Only the three NH-79 client-facing
    /// variants get their real message -- everything else (a raw
    /// `rusqlite`/`serde_json` error, or any other internal-only variant)
    /// collapses to a generic message, since `Display` on those can surface
    /// SQL text, column names, or other implementation detail that must
    /// never reach an HTTP response body. The full `Display` output is still
    /// what gets logged server-side via `tracing::error!(%err, ...)`.
    pub fn public_message(&self) -> String {
        match self {
            Self::CharacterNotFound
            | Self::CharacterNameInvalid(_)
            | Self::CharacterNameUnavailable => self.to_string(),
            _ => String::from("An internal error occurred"),
        }
    }
}

impl From<rusqlite::Error> for PersistenceError {
    fn from(error: rusqlite::Error) -> PersistenceError { PersistenceError::DatabaseError(error) }
}

impl From<serde_json::Error> for PersistenceError {
    fn from(error: serde_json::Error) -> PersistenceError {
        PersistenceError::SerializationError(error)
    }
}
