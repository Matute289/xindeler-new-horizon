-- Backfills the `spell_book` pseudo-container for every existing character.
--
-- Modelled on V60__recipe_book.sql, but deliberately NOT repeating its bug.
-- V60 carved out TWO id ranges from MAX(entity_id) — one for the recipe-book
-- containers and one for a default-recipe item inside each — and then inserted
-- only the FIRST range into `entity`. The second range therefore existed as
-- `item` rows whose ids had no `entity` row, which left SQLite's AUTOINCREMENT
-- sequence for `entity` BELOW the highest item_id in use. The next id the
-- server allocated (get_new_entity_ids reads `sqlite_sequence`, not
-- MAX(entity_id)) collided with those rows, and V61__fix_recipe_migration.sql
-- had to delete and re-create them.
--
-- Two rules follow from that, and this migration obeys both:
--   1. Allocate exactly ONE id per character and insert EVERY allocated id
--      into `entity` before any `item` row references it. A spell book starts
--      empty — there is no child item and so no second range to get wrong.
--   2. Base the offset on the highest id used ANYWHERE, not just MAX(entity_id),
--      so a database still carrying V60-style orphans cannot hand out an id that
--      an `item` row already occupies.

CREATE TEMP TABLE _temp_spell_book_pairings
(
    temp_row_id   INTEGER PRIMARY KEY AUTOINCREMENT NOT NULL,
    character_id  INT NOT NULL,
    spell_book_id INT
);

-- One row per character pseudo-container that does not already have a spell
-- book. The NOT EXISTS guard makes this a no-op for anything already migrated,
-- so re-running it can never produce a duplicate container (which the
-- idx_parent_container_item_id_position unique index would reject anyway).
INSERT
INTO _temp_spell_book_pairings
SELECT  NULL,
        i.item_id,
        NULL
FROM item i
WHERE i.item_definition_id = 'veloren.core.pseudo_containers.character'
AND NOT EXISTS (
    SELECT  1
    FROM    item s
    WHERE   s.parent_container_item_id = i.item_id
    AND     s.position = 'spell_book'
);

-- temp_row_id runs 1..N, so this hands out N consecutive unused ids. The
-- subquery is evaluated against the pre-insert state (nothing has been added to
-- `entity` yet), so it is constant across all rows.
UPDATE _temp_spell_book_pairings
SET spell_book_id = (
    SELECT MAX(
        (SELECT COALESCE(MAX(entity_id), 0) FROM entity),
        (SELECT COALESCE(MAX(item_id), 0) FROM item),
        (SELECT COALESCE(MAX(seq), 0) FROM sqlite_sequence WHERE name = 'entity')
    )
) + temp_row_id;

-- Register every id BEFORE it is referenced. This also advances the
-- AUTOINCREMENT sequence for `entity` past the whole range, which is the step
-- V60 missed for its second range.
INSERT
INTO entity (entity_id)
SELECT  t.spell_book_id
FROM    _temp_spell_book_pairings t;

-- The parent (the character pseudo-container) already exists and every id above
-- now has its `entity` row, so both foreign keys on `item` are satisfied at the
-- moment of insert; no deferral is needed.
INSERT
INTO item (item_id,
           parent_container_item_id,
           item_definition_id,
           stack_size,
           position,
           properties)
SELECT  t.spell_book_id,
        t.character_id,
        'veloren.core.pseudo_containers.spell_book',
        1,
        'spell_book',
        ''
FROM    _temp_spell_book_pairings t;

DROP TABLE _temp_spell_book_pairings;
