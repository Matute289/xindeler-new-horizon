//! Structs representing a playable Character

use crate::{comp, comp::inventory::Inventory};
use serde::{Deserialize, Serialize};

/// The limit on how many characters that a player can have
pub const MAX_CHARACTERS_PER_PLAYER: usize = 8;
#[derive(Copy, Clone, Debug, Serialize, Deserialize, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[serde(transparent)]
pub struct CharacterId(pub i64);

pub const MAX_NAME_LENGTH: usize = 20;

/// Server-side validation for `Character.alias` (NH-79). Historically this
/// field was never actually checked server-side — only the in-game
/// character-creator UI enforced `MAX_NAME_LENGTH` client-side (truncating
/// while typing) and a bare non-empty check. Any externally-reachable write
/// path (creation, the in-game editor, or the new rename endpoint) needs a
/// real check, since none of those guarantees survive a client that skips
/// the UI entirely.
///
/// Deliberately looser than `comp::player::Player::alias_validate` (which
/// governs the account-level display name, a different field entirely):
/// character names are commonly expected to allow spaces, accents and
/// apostrophes, so this allows any alphabetic Unicode character plus
/// digits, spaces, hyphens and apostrophes, rather than reusing that
/// validator's ASCII-only alphanumeric/`-`/`_` charset.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CharacterNameError {
    Empty,
    TooLong,
    ForbiddenCharacters,
}

/// Trims leading/trailing whitespace and collapses runs of internal
/// whitespace to a single space. Every write path must normalize *before*
/// validating and storing -- otherwise the `character.alias` uniqueness
/// index (`COLLATE NOCASE`, which does not fold whitespace) can be dodged by
/// visually-identical names that differ only in spacing (`" Aragorn"`,
/// `"Ara  gorn"` vs `"Aragorn"`), which matters here since character names
/// are player-facing display identity, not just a database key.
pub fn normalize_character_name(name: &str) -> String {
    name.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Validates an already-`normalize_character_name`d name. Passing an
/// un-normalized name (e.g. one with leading/trailing spaces) is a caller
/// bug, not something this function silently fixes -- normalize first, then
/// validate the same string you're about to store.
pub fn validate_character_name(name: &str) -> Result<(), CharacterNameError> {
    if name.is_empty() {
        Err(CharacterNameError::Empty)
    } else if name.chars().count() > MAX_NAME_LENGTH {
        Err(CharacterNameError::TooLong)
    } else if !name
        .chars()
        .all(|c| c.is_alphanumeric() || c == ' ' || matches!(c, '\'' | '-'))
    {
        Err(CharacterNameError::ForbiddenCharacters)
    } else {
        Ok(())
    }
}

/// The minimum character data we need to create a new character on the server.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Character {
    pub id: Option<CharacterId>,
    pub alias: String,
}

/// Data needed to render a single character item in the character list
/// presented during character selection.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CharacterItem<Location> {
    pub character: Character,
    pub body: comp::Body,
    pub hardcore: bool,
    pub inventory: Inventory,
    pub location: Option<Location>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_ordinary_names() {
        for name in ["Aragorn", "Mary-Jane", "O'Brien", "Élodie", "Two Words"] {
            assert_eq!(validate_character_name(name), Ok(()), "{name}");
        }
    }

    #[test]
    fn rejects_empty_or_whitespace_only() {
        for name in ["", "   "] {
            assert_eq!(
                validate_character_name(&normalize_character_name(name)),
                Err(CharacterNameError::Empty),
                "{name:?}"
            );
        }
    }

    #[test]
    fn normalize_trims_and_collapses_internal_whitespace() {
        for (input, expected) in [
            (" Aragorn", "Aragorn"),
            ("Aragorn ", "Aragorn"),
            ("Two   Words", "Two Words"),
            ("\tTabbed\tName\t", "Tabbed Name"),
            ("", ""),
            ("   ", ""),
        ] {
            assert_eq!(normalize_character_name(input), expected, "{input:?}");
        }
    }

    /// The whole point of normalizing before validating/storing: two names
    /// that only differ in spacing must normalize to the exact same string,
    /// so they can't dodge the `character.alias` uniqueness index by
    /// spacing alone.
    #[test]
    fn normalize_collapses_visual_duplicates_to_the_same_string() {
        assert_eq!(
            normalize_character_name(" Aragorn"),
            normalize_character_name("Aragorn  ")
        );
        assert_eq!(
            normalize_character_name("Ara  gorn"),
            normalize_character_name("Ara gorn")
        );
    }

    #[test]
    fn rejects_too_long() {
        let name = "a".repeat(MAX_NAME_LENGTH + 1);
        assert_eq!(
            validate_character_name(&name),
            Err(CharacterNameError::TooLong)
        );
        // Exactly at the limit is still fine.
        let name = "a".repeat(MAX_NAME_LENGTH);
        assert_eq!(validate_character_name(&name), Ok(()));
    }

    #[test]
    fn rejects_forbidden_characters() {
        for name in ["Bad@Name", "Sneaky<script>", "semicolon;here"] {
            assert_eq!(
                validate_character_name(name),
                Err(CharacterNameError::ForbiddenCharacters),
                "{name}"
            );
        }
    }
}
