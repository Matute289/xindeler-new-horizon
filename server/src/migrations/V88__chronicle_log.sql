-- Durable backing for the in-memory ChronicleLog stand-in (BL-88 / xindeler-zuul ZG-36) -- every
-- game-server restart previously wiped ORACLE's narrative history outright. Append-only, pruned to
-- ChronicleLog::bounds::MAX_ENTRIES (1024) on every insert -- this table is a rolling window
-- mirroring the in-memory cap, not a permanent audit log; see chronicle.rs's own doc comment for
-- why (explicit stand-in for a future, richer chronicle/lore system, not that system itself).
CREATE TABLE chronicle_log
(
    id          INTEGER NOT NULL PRIMARY KEY AUTOINCREMENT,
    text        TEXT    NOT NULL,
    -- ISO8601 (chrono::DateTime<Utc>::to_rfc3339()), same convention as
    -- character_suspensions.suspended_at/end_date.
    created_at  TEXT    NOT NULL
);
