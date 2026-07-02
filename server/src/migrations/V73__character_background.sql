-- BL-31: per-character background (comp::Background / BackgroundKind).
-- Nullable: existing characters load as Background(None) ("Uncommitted",
-- P0 §Q1) with no forced choice and no data loss.
-- `background_custom_note` only populated when background = 'custom'
-- (P0 §Q5: DM-approved freeform text, max 200 chars enforced in code).
ALTER TABLE "character" ADD COLUMN background TEXT;
ALTER TABLE "character" ADD COLUMN background_custom_note TEXT;
