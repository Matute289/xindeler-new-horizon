-- Per-character suspension: freezes a single character (rejects selection,
-- kicks its live session if playing) without touching the rest of that
-- player's account -- distinct from an account-level ban.
--
-- One row per currently-suspended character (PRIMARY KEY on character_id, not
-- an append-only log): an unsuspend deletes the row outright, and a
-- re-suspend replaces it. A boolean flag on `character` couldn't carry the
-- audit fields this needs (who, when, why, until when), so this is a real
-- table rather than following the `hardcore` column precedent directly.
CREATE TABLE character_suspensions
(
    character_id                INTEGER NOT NULL
        PRIMARY KEY
        REFERENCES "character"(character_id),
    reason                      TEXT NOT NULL,
    suspended_by_operator_uuid  TEXT NOT NULL,
    suspended_at                TEXT NOT NULL,
    -- ISO8601 (chrono::DateTime<Utc>::to_rfc3339). NULL means permanent, i.e.
    -- lasts until a manual unsuspend -- never "an omitted duration", since the
    -- admin-facing API requires an explicit duration_secs up front.
    end_date                    TEXT
);
