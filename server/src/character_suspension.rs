//! In-memory cache of every currently-suspended character, mirroring how
//! `EditableSettings`/`Banlist` cache their own durable state as an ECS
//! resource -- except this one is backed by the `character_suspensions`
//! table rather than a settings file, since suspension needs audit fields
//! (who, when, why, until when) a settings file's usual shape doesn't carry
//! as naturally as a real table.

use crate::persistence::{self, DatabaseSettings, SuspensionRecord};
use chrono::{DateTime, Utc};
use common::character::CharacterId;
use hashbrown::HashMap;
use tracing::error;

/// Every character currently suspended, keyed by `CharacterId`. Loaded once at
/// server startup and kept in sync by every suspend/unsuspend admin action
/// afterwards -- never re-read from the database mid-session, the same
/// contract `EditableSettings` already keeps for its own file-backed state.
///
/// Expiry is checked lazily against the wall clock at the point of use
/// (`current`, below) rather than through a periodic sweep. An entry whose
/// `end_date` has passed still physically sits in the map (and its database
/// row still exists) until an explicit unsuspend clears it, but `current`
/// stops treating it as active the moment its deadline passes -- which is
/// all character-select enforcement actually needs.
#[derive(Default)]
pub struct CharacterSuspensions(HashMap<CharacterId, SuspensionRecord>);

impl CharacterSuspensions {
    /// Reads every currently-suspended character from the database. Called
    /// once at server startup; a read failure is logged and treated as
    /// "nothing is suspended" rather than aborting startup entirely -- an
    /// admin can always re-suspend, but a startup panic here would take down
    /// the whole server over a moderation cache failing to warm.
    pub fn load(settings: &DatabaseSettings) -> Self {
        match persistence::load_all_character_suspensions(settings) {
            Ok(map) => Self(map),
            Err(err) => {
                error!(
                    ?err,
                    "Failed to load character suspensions from the database; starting with none \
                     cached"
                );
                Self::default()
            },
        }
    }

    /// The active suspension on `character_id` right now, or `None` if it was
    /// never suspended, or was but its `end_date` has already passed.
    pub fn current(
        &self,
        character_id: CharacterId,
        now: DateTime<Utc>,
    ) -> Option<&SuspensionRecord> {
        self.0
            .get(&character_id)
            .filter(|record| record.end_date.is_none_or(|end| end > now))
    }

    /// Records a fresh (or replacement) suspension in the cache. Called right
    /// after the matching database write succeeds, so the two never drift.
    pub(crate) fn insert(&mut self, character_id: CharacterId, record: SuspensionRecord) {
        self.0.insert(character_id, record);
    }

    /// Clears `character_id`'s suspension from the cache. Called right after
    /// the matching database delete succeeds. Idempotent, same as the
    /// database side.
    pub(crate) fn remove(&mut self, character_id: CharacterId) { self.0.remove(&character_id); }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn record(end_date: Option<DateTime<Utc>>) -> SuspensionRecord {
        SuspensionRecord {
            reason: "test".to_owned(),
            suspended_by_operator_uuid: "operator".to_owned(),
            suspended_at: Utc::now(),
            end_date,
        }
    }

    #[test]
    fn a_permanent_suspension_is_always_current() {
        let mut suspensions = CharacterSuspensions::default();
        let id = CharacterId(1);
        suspensions.insert(id, record(None));

        assert!(suspensions.current(id, Utc::now()).is_some());
    }

    #[test]
    fn an_expired_suspension_is_no_longer_current_but_stays_cached() {
        let mut suspensions = CharacterSuspensions::default();
        let id = CharacterId(1);
        let now = Utc::now();
        suspensions.insert(id, record(Some(now - chrono::Duration::seconds(1))));

        assert!(
            suspensions.current(id, now).is_none(),
            "an expired suspension must not be enforced"
        );
        assert!(
            suspensions.0.contains_key(&id),
            "expiry is lazy -- the entry is not evicted just by checking it"
        );
    }

    #[test]
    fn a_future_end_date_is_still_current() {
        let mut suspensions = CharacterSuspensions::default();
        let id = CharacterId(1);
        let now = Utc::now();
        suspensions.insert(id, record(Some(now + chrono::Duration::seconds(60))));

        assert!(suspensions.current(id, now).is_some());
    }

    #[test]
    fn removing_clears_it() {
        let mut suspensions = CharacterSuspensions::default();
        let id = CharacterId(1);
        suspensions.insert(id, record(None));
        suspensions.remove(id);

        assert!(suspensions.current(id, Utc::now()).is_none());
    }

    #[test]
    fn an_unsuspended_character_has_no_current_suspension() {
        let suspensions = CharacterSuspensions::default();
        assert!(suspensions.current(CharacterId(1), Utc::now()).is_none());
    }
}
