-- The Blade boon's own experience track (comp::Pact::blade_exp/blade_name).
--
-- Both columns are nullable, same fail-open pattern as every other pact
-- column: NULL -> 0 / not chosen yet, and a non-Blade character (or any
-- character predating this migration) is completely unaffected.
--
-- `pact_blade_exp` is cumulative XP earned while `pact_blade_summoned` was
-- true; it survives a boon change (re-taking Blade resumes from here rather
-- than resetting).
--
-- `pact_blade_name` is the name the blade chose for itself on reaching tier
-- 5 -- picked server-side from a curated per-alignment list
-- (assets/common/pact/blade_names.ron), never player-typed text.
ALTER TABLE "character" ADD COLUMN pact_blade_exp INTEGER;
ALTER TABLE "character" ADD COLUMN pact_blade_name TEXT;
