-- Per-character Warlock pact state (comp::Pact / PactStanding / PatronId).
--
-- All three columns are nullable: an existing character loads with no pact
-- row data, which the code reads back as Pact::default() (Bound, no
-- patron chosen yet) -- no forced choice and no data loss, same pattern as
-- `background`.
--
-- `pact_favour` is reserved for a future demand/favour mechanic. It rides
-- this migration now so a later feature doesn't need its own schema change,
-- but nothing reads or writes it yet -- every row gets 0.
ALTER TABLE "character" ADD COLUMN pact_standing TEXT;
ALTER TABLE "character" ADD COLUMN pact_patron_id TEXT;
ALTER TABLE "character" ADD COLUMN pact_favour INTEGER;
