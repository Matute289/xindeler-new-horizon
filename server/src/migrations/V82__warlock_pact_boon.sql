-- Which of the four canon Pact Boons (Tome/Chain/Blade/Talisman) a
-- character's pact grants, orthogonal to the patron and standing columns
-- added in V81.
--
-- Nullable: NULL -> no boon chosen yet, same fail-open pattern as every
-- other pact column. Existing characters (and every character created
-- before this shipped) load with no boon and no data loss.
ALTER TABLE "character" ADD COLUMN pact_boon TEXT;
