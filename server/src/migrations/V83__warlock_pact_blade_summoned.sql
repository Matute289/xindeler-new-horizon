-- Whether a Blade-boon Warlock currently has their conjured weapon out.
-- Meaningless for every other boon.
--
-- Nullable: NULL -> false (not summoned), same fail-open pattern as every
-- other pact column. Stored as an integer (0/1), matching `hardcore`'s
-- existing boolean-as-integer convention on this table.
ALTER TABLE "character" ADD COLUMN pact_blade_summoned INTEGER;
