-- Cache for the lazily-rendered character portraits the web account page shows.
-- The portrait itself is drawn by the `portrait_gen` subprocess out of
-- the character's persisted body and loadout; this table exists purely so that
-- render runs about once per loadout change per character instead of once per
-- page view.
--
-- One row per character (PRIMARY KEY on character_id, replaced on
-- regeneration), so the table is bounded by the character table itself:
-- MAX_CHARACTERS_PER_PLAYER (8) rows per account, each holding a ~10-40 KB
-- encoded image. Every row is disposable -- dropping one costs the next reader
-- one re-render and nothing else -- which is why this is a cache and not an
-- audit trail, and why nothing in the server ever fails because a row is
-- missing.
--
-- Deliberately NOT declared `REFERENCES "character"(character_id)`, unlike
-- character_suspensions (V87). Every connection this server opens runs with
-- `PRAGMA foreign_keys = ON` (persistence/mod.rs's establish_connection), so a
-- declared foreign key would make `DELETE FROM character` fail outright for any
-- character that still had a cached portrait -- i.e. it would let a cosmetic
-- cache block character deletion. `delete_character` deletes the row itself,
-- inside the same transaction and before the character row goes, which is the
-- guarantee that actually matters here.
CREATE TABLE character_portraits
(
    character_id    INTEGER NOT NULL PRIMARY KEY,   -- character.character_id, by convention (see above)
    -- The versioned canonical appearance string built by server/src/portrait.rs
    -- from the persisted body plus the visible equipment slots. Stored as the
    -- string itself rather than a hash: it is under a kilobyte, it is what
    -- makes a "why is this portrait stale" report diagnosable by reading the
    -- column, and a hash would have to be a stable one anyway. Its leading
    -- `p1|` is the renderer's params version, so bumping that constant
    -- invalidates every row lazily without a migration.
    appearance_key  TEXT    NOT NULL,
    -- Encoding of `image`, as the subtype of its image/* media type ('webp'
    -- today). A plain TEXT column rather than an enum or a CHECK constraint:
    -- the WebP-vs-PNG measurement came out close enough that reverting stays
    -- on the table, and it must not need a migration when it happens.
    format          TEXT    NOT NULL,
    image           BLOB    NOT NULL,
    -- ISO8601 (chrono::DateTime<Utc>::to_rfc3339()), the same convention as
    -- character_suspensions.suspended_at and chronicle_log.created_at. Written
    -- for operators reading the database directly; staleness is decided by
    -- appearance_key, never by age, so nothing in the server reads it back.
    rendered_at     TEXT    NOT NULL
);
