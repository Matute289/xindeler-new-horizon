-- Make all character names longer than 20 characters be 20 characters.
--
-- In code we also limit it to 20 characters with `common::character::MAX_NAME_LENGTH`,
-- enforced by `common::character::validate_character_name` on every write path
-- (create / edit / rename).
--
-- Two deviations from the version this came from upstream:
--
--   1. The version number is V90, not V71: V71 is already taken here by
--      V71__character_class.sql, and refinery keys migrations by version, so a
--      duplicate number aborts startup.
--
--   2. The UPDATE is guarded. This database has a UNIQUE index on
--      `character.alias COLLATE NOCASE` (V86), which the upstream version does
--      not; truncating two names that share their first 20 characters would
--      collide and abort the migration, taking the server down with it. Rows
--      whose truncation would collide with an existing alias, or with another
--      row being truncated in the same pass, are left as they are — they are
--      already unreachable by the validator on any write, so leaving them
--      unchanged is strictly safer than failing to boot.
UPDATE "character"
   SET alias = substr(alias, 1, 20)
 WHERE length(alias) > 20
   AND NOT EXISTS (
       SELECT 1
         FROM "character" AS other
        WHERE other.rowid <> "character".rowid
          AND other.alias = substr("character".alias, 1, 20) COLLATE NOCASE
   )
   AND NOT EXISTS (
       SELECT 1
         FROM "character" AS other
        WHERE other.rowid <> "character".rowid
          AND length(other.alias) > 20
          AND substr(other.alias, 1, 20) = substr("character".alias, 1, 20) COLLATE NOCASE
   );
