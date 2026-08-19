-- Case-insensitive uniqueness (NH-79, spec §4.4): NOCASE is ASCII-only case
-- folding in SQLite, not full Unicode case folding — accented letters that
-- only differ by case (e.g. "É" vs "é") are still treated as distinct. This
-- matches the ASCII-only normalization xindeler-auth already applies to
-- account usernames elsewhere in this project.
CREATE UNIQUE INDEX idx_character_alias_unique ON character(alias COLLATE NOCASE);
