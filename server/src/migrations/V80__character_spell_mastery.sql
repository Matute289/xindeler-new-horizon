-- Per-`MagicSource` mastery progress, as a small JSON object of
-- source -> accumulated source-XP.
--
-- Nullable on purpose: every existing character loads as "nothing accrued
-- yet" (all sources at 0%), with no forced choice and no data loss. `Arcane`
-- is never written (known by default) and so never appears in the JSON.
ALTER TABLE "character" ADD COLUMN spell_mastery TEXT;
