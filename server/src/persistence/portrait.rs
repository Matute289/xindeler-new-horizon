//! The `character_portraits` cache, and the read that feeds it.
//!
//! Deliberately column-shaped rather than model-shaped (no `models.rs`
//! struct), like `chronicle.rs`: the table is four scalar columns plus a blob,
//! and the only consumer is `crate::portrait`. The style otherwise mirrors
//! `character/mod.rs`'s suspension helpers -- raw `rusqlite`,
//! `prepare_cached`, `PersistenceError` via `?`, transactions owned by the
//! caller.
//!
//! Every row here is disposable: it can be deleted at any time and the only
//! consequence is that the next reader pays for a re-render. Nothing in the
//! server treats a missing row, or a failed write to this table, as an error
//! worth failing a request over.

extern crate rusqlite;

use super::{
    character::{
        convert_body_from_database, convert_inventory_from_database_items, get_pseudo_containers,
        load_items,
    },
    error::PersistenceError,
};
use chrono::{DateTime, Utc};
use common::{
    character::CharacterId,
    comp::{Body, Inventory},
};
use rusqlite::{Connection, OptionalExtension, Transaction};

/// A cached portrait as it is stored, minus `rendered_at`.
///
/// `rendered_at` is written for operators reading the database directly and is
/// deliberately not selected back: staleness here is decided by comparing
/// `appearance_key` against the character's current appearance, never by the
/// row's age, so reading the timestamp would only add a parse that could fail
/// on a column nothing depends on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PortraitRow {
    /// The appearance this image was rendered from. A row whose key differs
    /// from the character's current one is stale and must be replaced, not
    /// served.
    pub appearance_key: String,
    /// Subtype of the image's `image/*` media type, e.g. `"webp"`.
    pub format: String,
    /// The encoded image.
    pub image: Vec<u8>,
}

/// The cached portrait for `character_id`, if one has ever been rendered.
///
/// Does no ownership check of its own: the caller has already had to load the
/// character's body and inventory through [`load_portrait_inputs`], which does,
/// and cannot produce a `character_id` it does not own.
pub fn get_portrait(
    character_id: CharacterId,
    connection: &Connection,
) -> Result<Option<PortraitRow>, PersistenceError> {
    let mut stmt = connection.prepare_cached(
        "
        SELECT  appearance_key,
                format,
                image
        FROM    character_portraits
        WHERE   character_id = ?1",
    )?;

    let row = stmt
        .query_row([character_id.0], |row| {
            Ok(PortraitRow {
                appearance_key: row.get(0)?,
                format: row.get(1)?,
                image: row.get(2)?,
            })
        })
        .optional()?;

    Ok(row)
}

/// Stores the portrait for `character_id`, replacing whatever was cached
/// before.
///
/// `REPLACE INTO` on the single-column primary key, the same shape
/// `suspend_character` uses: a character has one portrait, and re-rendering
/// overwrites it rather than accumulating history.
pub fn upsert_portrait(
    character_id: CharacterId,
    appearance_key: &str,
    format: &str,
    image: &[u8],
    rendered_at: DateTime<Utc>,
    transaction: &mut Transaction,
) -> Result<(), PersistenceError> {
    let mut stmt = transaction.prepare_cached(
        "
        REPLACE
        INTO    character_portraits (character_id, appearance_key, format, image, rendered_at)
        VALUES  (?1, ?2, ?3, ?4, ?5)",
    )?;

    stmt.execute(rusqlite::params![
        character_id.0,
        appearance_key,
        format,
        image,
        rendered_at.to_rfc3339(),
    ])?;
    Ok(())
}

/// Drops any cached portrait for `character_id`.
///
/// Idempotent -- deleting a row that was never rendered is not an error, the
/// same way `unsuspend_character` treats a character that was never suspended.
/// Called from inside `delete_character`'s transaction, so a deleted character
/// can never leave its portrait behind.
pub fn delete_portrait(
    character_id: CharacterId,
    transaction: &mut Transaction,
) -> Result<(), PersistenceError> {
    let mut stmt = transaction.prepare_cached(
        "
        DELETE
        FROM    character_portraits
        WHERE   character_id = ?1",
    )?;

    stmt.execute([character_id.0])?;
    Ok(())
}

/// The persisted body and inventory a portrait is rendered from, for one of
/// `player_uuid`'s own characters.
///
/// The ownership predicate is in the SQL rather than left to the caller (the
/// `rename_character` pattern), and it runs *before* any of the item loads, so
/// a `character_id` belonging to someone else costs one indexed lookup and
/// reveals nothing: a character that does not exist and a character owned by
/// somebody else are the same `CharacterNotFound`, which is what keeps the
/// endpoint above this from being an enumeration oracle.
///
/// This is `load_character_data`'s body/inventory half and nothing else -- no
/// skills, pets, ability sets or waypoint, none of which affect what a
/// character looks like.
pub fn load_portrait_inputs(
    character_id: CharacterId,
    player_uuid: &str,
    connection: &Connection,
) -> Result<(Body, Inventory), PersistenceError> {
    let mut stmt = connection.prepare_cached(
        "
        SELECT  b.variant,
                b.body_data
        FROM    character c
        JOIN    body b ON (c.character_id = b.body_id)
        WHERE   c.player_uuid = ?1
        AND     c.character_id = ?2",
    )?;

    let (variant, body_data) = stmt
        .query_row(rusqlite::params![player_uuid, character_id.0], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })
        .optional()?
        .ok_or(PersistenceError::CharacterNotFound)?;
    drop(stmt);

    let body = convert_body_from_database(&variant, &body_data)?;

    let containers = get_pseudo_containers(connection, character_id)?;
    let inventory_items = load_items(connection, containers.inventory_container_id)?;
    let loadout_items = load_items(connection, containers.loadout_container_id)?;
    let overflow_items = load_items(connection, containers.overflow_items_container_id)?;
    let recipe_book_items = load_items(connection, containers.recipe_book_container_id)?;
    let spell_book_items = load_items(connection, containers.spell_book_container_id)?;

    let inventory = convert_inventory_from_database_items(
        containers.inventory_container_id,
        &inventory_items,
        containers.loadout_container_id,
        &loadout_items,
        containers.overflow_items_container_id,
        &overflow_items,
        &recipe_book_items,
        &spell_book_items,
    )?;

    Ok((body, inventory))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::persistence::{
        ConnectionMode, DatabaseSettings, PersistedComponents, SqlLogMode, VelorenConnection,
        establish_connection,
    };
    use common::{
        comp::{
            self, ActiveAbilities, CharacterClass,
            slot::{ArmorSlot, EquipSlot},
        },
        resources::Time,
    };
    use common_i18n::Content;

    const OWNER: &str = "11111111-1111-1111-1111-111111111111";
    const STRANGER: &str = "22222222-2222-2222-2222-222222222222";
    const CHEST: &str = "common.items.armor.cloth_blue.chest";

    struct TestDb {
        _dir: tempfile::TempDir,
        settings: DatabaseSettings,
    }

    impl TestDb {
        /// A fresh database with every migration applied, exactly as a server
        /// would produce on first start.
        fn new() -> Self {
            let dir = tempfile::tempdir().expect("temp dir");
            let settings = DatabaseSettings {
                db_dir: dir.path().to_path_buf(),
                sql_log_mode: SqlLogMode::Disabled,
            };
            crate::persistence::run_migrations(&settings);
            Self {
                _dir: dir,
                settings,
            }
        }

        fn connection(&self) -> VelorenConnection {
            establish_connection(&self.settings, ConnectionMode::ReadWrite)
        }

        fn store(&self, character_id: CharacterId, key: &str, image: &[u8]) {
            let mut conn = self.connection();
            let mut transaction = conn.connection.transaction().expect("transaction");
            upsert_portrait(
                character_id,
                key,
                "webp",
                image,
                Utc::now(),
                &mut transaction,
            )
            .expect("upsert");
            transaction.commit().expect("commit");
        }

        fn fetch(&self, character_id: CharacterId) -> Option<PortraitRow> {
            get_portrait(character_id, &self.connection()).expect("get")
        }

        fn drop_row(&self, character_id: CharacterId) {
            let mut conn = self.connection();
            let mut transaction = conn.connection.transaction().expect("transaction");
            delete_portrait(character_id, &mut transaction).expect("delete");
            transaction.commit().expect("commit");
        }
    }

    fn components(inventory: Inventory) -> PersistedComponents {
        let body = comp::Body::Humanoid(comp::humanoid::Body::random());
        PersistedComponents {
            body,
            hardcore: None,
            character_class: CharacterClass::single(comp::ClassKind::Mage),
            stats: comp::Stats::new(Content::Plain("Testificate".to_owned()), body),
            skill_set: comp::SkillSet::default(),
            inventory,
            waypoint: None,
            pets: Vec::new(),
            active_abilities: ActiveAbilities::default(),
            map_marker: None,
            ethos: comp::Ethos::default(),
            background: comp::Background::default(),
            pact: comp::Pact::default(),
            trigger_slots: comp::TriggerSlots::default(),
            spell_mastery: comp::SpellMastery::default(),
        }
    }

    /// Creates a character owned by `uuid`, wearing `chest` if given, and
    /// returns its id.
    fn create(db: &TestDb, uuid: &str, alias: &str, chest: Option<&str>) -> CharacterId {
        let mut inventory = Inventory::with_empty();
        if let Some(specifier) = chest {
            inventory.replace_loadout_item(
                EquipSlot::Armor(ArmorSlot::Chest),
                Some(comp::Item::new_from_asset_expect(specifier)),
                Time(0.0),
            );
        }

        let mut conn = db.connection();
        let mut transaction = conn.connection.transaction().expect("transaction");
        let (id, _) = crate::persistence::character::create_character(
            uuid,
            alias,
            components(inventory),
            &mut transaction,
        )
        .expect("character creation");
        transaction.commit().expect("commit");
        id
    }

    #[test]
    fn an_uncached_character_has_no_portrait() {
        let db = TestDb::new();
        assert_eq!(db.fetch(CharacterId(1)), None);
    }

    #[test]
    fn upsert_replaces_rather_than_accumulating() {
        let db = TestDb::new();
        let id = CharacterId(7);

        db.store(id, "p1|first", b"first-image");
        db.store(id, "p1|second", b"second-image");

        let row = db.fetch(id).expect("a portrait is cached");
        assert_eq!(row.appearance_key, "p1|second");
        assert_eq!(row.image, b"second-image");
        assert_eq!(row.format, "webp");

        let count: i64 = db
            .connection()
            .query_row("SELECT COUNT(1) FROM character_portraits", [], |r| r.get(0))
            .expect("count");
        assert_eq!(count, 1, "a re-render must replace the row, not add one");
    }

    #[test]
    fn delete_is_idempotent() {
        let db = TestDb::new();
        let id = CharacterId(7);

        db.drop_row(id);
        db.store(id, "p1|only", b"image");
        db.drop_row(id);
        db.drop_row(id);

        assert_eq!(db.fetch(id), None);
    }

    #[test]
    fn a_blob_survives_the_round_trip_byte_for_byte() {
        let db = TestDb::new();
        let id = CharacterId(3);
        // Bytes that a TEXT column would mangle: a NUL, and something that is
        // not valid UTF-8. An encoded image is full of both.
        let image: Vec<u8> = vec![0x52, 0x49, 0x46, 0x46, 0x00, 0xff, 0xfe, 0x00, 0x80];

        db.store(id, "p1|binary", &image);

        assert_eq!(db.fetch(id).expect("cached").image, image);
    }

    #[test]
    fn portrait_inputs_load_the_persisted_body_and_loadout() {
        let db = TestDb::new();
        let id = create(&db, OWNER, "Testificate", Some(CHEST));

        let (body, inventory) =
            load_portrait_inputs(id, OWNER, &db.connection()).expect("the owner can load");

        assert!(matches!(body, Body::Humanoid(_)));
        let chest = inventory
            .equipped(EquipSlot::Armor(ArmorSlot::Chest))
            .expect("the chest piece was persisted");
        assert_eq!(
            chest.item_definition_id().itemdef_id(),
            Some(CHEST),
            "the loadout must come back as it was saved"
        );
    }

    #[test]
    fn portrait_inputs_reject_someone_elses_character() {
        let db = TestDb::new();
        let id = create(&db, OWNER, "Testificate", None);

        let err = load_portrait_inputs(id, STRANGER, &db.connection())
            .expect_err("a stranger must not be able to load this character");
        assert!(
            matches!(err, PersistenceError::CharacterNotFound),
            "ownership failure must be indistinguishable from a missing character, got {err:?}"
        );
    }

    #[test]
    fn portrait_inputs_reject_a_character_that_does_not_exist() {
        let db = TestDb::new();
        create(&db, OWNER, "Testificate", None);

        let err = load_portrait_inputs(CharacterId(987_654), OWNER, &db.connection())
            .expect_err("a character that was never created must not load");
        assert!(matches!(err, PersistenceError::CharacterNotFound));
    }

    #[test]
    fn deleting_a_character_takes_its_portrait_with_it() {
        let db = TestDb::new();
        let id = create(&db, OWNER, "Testificate", None);
        db.store(id, "p1|cached", b"image");
        assert!(db.fetch(id).is_some());

        let mut conn = db.connection();
        let mut transaction = conn.connection.transaction().expect("transaction");
        crate::persistence::character::delete_character(OWNER, id, &mut transaction)
            .expect("the owner can delete their own character");
        transaction.commit().expect("commit");

        assert_eq!(
            db.fetch(id),
            None,
            "a deleted character must not leave a cached portrait behind"
        );
    }

    #[test]
    fn a_refused_character_deletion_leaves_the_portrait_alone() {
        let db = TestDb::new();
        let id = create(&db, OWNER, "Testificate", None);
        db.store(id, "p1|cached", b"image");

        let mut conn = db.connection();
        let mut transaction = conn.connection.transaction().expect("transaction");
        // `delete_character` silently no-ops for a character the requester
        // doesn't own; the portrait deletion must sit behind that same check.
        crate::persistence::character::delete_character(STRANGER, id, &mut transaction)
            .expect("a stranger's delete is a silent no-op, not an error");
        transaction.commit().expect("commit");

        assert!(
            db.fetch(id).is_some(),
            "a stranger must not be able to evict another player's cached portrait"
        );
    }
}
