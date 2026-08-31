//! Database operations related to character data
//!
//! Methods in this module should remain private to the persistence module -
//! database updates and loading are communicated via requests to the
//! [`CharacterLoader`] and [`CharacterUpdater`] while results/responses are
//! polled and handled each server tick.
extern crate rusqlite;

use super::{error::PersistenceError, models::*};
use crate::{
    comp::{self, Inventory, MapMarker, Waypoint},
    persistence::{
        EditableComponents, PersistedComponents,
        character::conversions::{
            convert_active_abilities_from_database, convert_active_abilities_to_database,
            convert_background_from_database, convert_background_to_database,
            convert_body_to_database_json, convert_character_from_database,
            convert_class_from_database, convert_class_to_database, convert_ethos_from_database,
            convert_future_levels_to_secondary_to_database, convert_hardcore_from_database,
            convert_hardcore_to_database, convert_inventory_from_database_items,
            convert_items_to_database_items, convert_pact_from_database, convert_pact_to_database,
            convert_recipe_book_from_database_items, convert_secondary_class_level_to_database,
            convert_secondary_class_to_database, convert_skill_groups_to_database,
            convert_skill_set_from_database, convert_stats_from_database,
            convert_waypoint_to_database_json,
        },
        character_loader::{CharacterCreationResult, CharacterDataResult, CharacterListResult},
        character_updater::PetPersistenceData,
        error::PersistenceError::DatabaseError,
        json_models,
    },
};
use chrono::{DateTime, Utc};
use common::{
    character::{
        CharacterId, CharacterItem, CharacterSuspension, MAX_CHARACTERS_PER_PLAYER,
        normalize_character_name, validate_character_name,
    },
    comp::{Content, skillset::level_from_total_exp},
    event::{PermanentChange, UpdateCharacterMetadata},
    npc::NPC_NAMES,
};
use core::ops::Range;
use hashbrown::HashMap;
use rusqlite::{Connection, Error as DbError, ErrorCode, ToSql, Transaction, types::Value};
use std::{num::NonZeroU64, rc::Rc};
use tracing::{debug, error, trace, warn};

/// Private module for very tightly coupled database conversion methods.  In
/// general, these have many invariants that need to be maintained when they're
/// called--do not assume it's safe to make these public!
mod conversions;

/// `pub(crate)`, not `pub(in crate::persistence)`: this module itself is
/// only `pub(in crate::persistence)`, but the actual caller,
/// `Server::resolve_waypoint_site_name`, lives at the crate root in
/// `server/src/lib.rs` -- outside `crate::persistence` entirely -- and
/// re-exports can never widen visibility past their target's own, so
/// `persistence::mod.rs`'s own `pub(crate) use character::
/// convert_waypoint_from_database_json as parse_waypoint;` requires this to
/// already be `pub(crate)` here, not narrower. `resolve_waypoint_site_name`
/// needs to parse a raw waypoint JSON string with no `CharacterId` on hand to
/// log against on a parse failure -- unlike every in-module caller, which
/// goes through `convert_waypoint_or_warn` instead.
pub(crate) use conversions::convert_waypoint_from_database_json;

/// `persistence::portrait::load_portrait_inputs` rebuilds the body and the
/// *equipped* half of what `load_character_data` rebuilds, for a character
/// whose player may not be connected. It lives in `persistence/portrait.rs`
/// rather than here so that the portrait cache doesn't grow inside this
/// (upstream-hot) file. Widened to `pub(in crate::persistence)` and no further
/// -- the module warning above still stands for everyone outside
/// `persistence`. The invariant these carry is the topological ordering of the
/// item rows they are handed, which is `load_items`' job (already `pub`), and
/// `load_portrait_inputs` feeds them the same way `load_character_data` does.
pub(in crate::persistence) use conversions::{
    convert_body_from_database, convert_loadout_from_database_items,
};

pub(crate) type EntityId = i64;

const CHARACTER_PSEUDO_CONTAINER_DEF_ID: &str = "veloren.core.pseudo_containers.character";
const INVENTORY_PSEUDO_CONTAINER_DEF_ID: &str = "veloren.core.pseudo_containers.inventory";
const LOADOUT_PSEUDO_CONTAINER_DEF_ID: &str = "veloren.core.pseudo_containers.loadout";
const OVERFLOW_ITEMS_PSEUDO_CONTAINER_DEF_ID: &str =
    "veloren.core.pseudo_containers.overflow_items";
const RECIPE_BOOK_PSEUDO_CONTAINER_DEF_ID: &str = "veloren.core.pseudo_containers.recipe_book";
/// Xindeler: holds the character's learned `ItemKind::SpellGroup` items. Every
/// character has one, backfilled for pre-existing characters by the
/// `spell_book` migration.
const SPELL_BOOK_PSEUDO_CONTAINER_DEF_ID: &str = "veloren.core.pseudo_containers.spell_book";
const INVENTORY_PSEUDO_CONTAINER_POSITION: &str = "inventory";
/// `pub(in crate::persistence)` for `persistence::portrait`, which resolves
/// this one container and none of the others.
pub(in crate::persistence) const LOADOUT_PSEUDO_CONTAINER_POSITION: &str = "loadout";
const OVERFLOW_ITEMS_PSEUDO_CONTAINER_POSITION: &str = "overflow_items";
const RECIPE_BOOK_PSEUDO_CONTAINER_POSITION: &str = "recipe_book";
const SPELL_BOOK_PSEUDO_CONTAINER_POSITION: &str = "spell_book";
const WORLD_PSEUDO_CONTAINER_ID: EntityId = 1;

#[derive(Clone, Copy)]
struct CharacterContainers {
    inventory_container_id: EntityId,
    loadout_container_id: EntityId,
    overflow_items_container_id: EntityId,
    recipe_book_container_id: EntityId,
    spell_book_container_id: EntityId,
}

/// Load the inventory/loadout
///
/// Loading is done recursively to ensure that each is topologically sorted in
/// the sense required by convert_inventory_from_database_items.
///
/// For items with components, the parent item must sorted so that its
/// components are after the parent item.
pub fn load_items(connection: &Connection, root: i64) -> Result<Vec<Item>, PersistenceError> {
    let mut stmt = connection.prepare_cached(
        "
        WITH RECURSIVE
        items_tree (
            item_id,
            parent_container_item_id,
            item_definition_id,
            stack_size,
            position,
            properties
        ) AS (
            SELECT  item_id,
                    parent_container_item_id,
                    item_definition_id,
                    stack_size,
                    position,
                    properties
            FROM item
            WHERE parent_container_item_id = ?1
            UNION ALL
            SELECT  item.item_id,
                    item.parent_container_item_id,
                    item.item_definition_id,
                    item.stack_size,
                    item.position,
                    item.properties
            FROM item, items_tree
            WHERE item.parent_container_item_id = items_tree.item_id
        )
        SELECT  *
        FROM    items_tree",
    )?;

    let items = stmt
        .query_map([root], |row| {
            Ok(Item {
                item_id: row.get(0)?,
                parent_container_item_id: row.get(1)?,
                item_definition_id: row.get(2)?,
                stack_size: row.get(3)?,
                position: row.get(4)?,
                properties: row.get(5)?,
            })
        })?
        .filter_map(Result::ok)
        .collect::<Vec<Item>>();

    Ok(items)
}

fn convert_waypoint_or_warn(
    waypoint_json: Option<&str>,
    char_id: CharacterId,
) -> (Option<Waypoint>, Option<MapMarker>) {
    match waypoint_json.map(convert_waypoint_from_database_json) {
        Some(Ok(w)) => w,
        Some(Err(e)) => {
            warn!(
                "Error reading waypoint from database for character ID
    {}, error: {}",
                char_id.0, e
            );
            (None, None)
        },
        None => (None, None),
    }
}

/// Load stored data for a character.
///
/// After first logging in, and after a character is selected, we fetch this
/// data for the purpose of inserting their persisted data for the entity.
pub fn load_character_data(
    requesting_player_uuid: String,
    char_id: CharacterId,
    connection: &Connection,
) -> CharacterDataResult {
    let character_containers = get_pseudo_containers(connection, char_id)?;
    let inventory_items = load_items(connection, character_containers.inventory_container_id)?;
    let loadout_items = load_items(connection, character_containers.loadout_container_id)?;
    let overflow_items_items =
        load_items(connection, character_containers.overflow_items_container_id)?;
    let recipe_book_items = load_items(connection, character_containers.recipe_book_container_id)?;
    let spell_book_items = load_items(connection, character_containers.spell_book_container_id)?;

    let mut stmt = connection.prepare_cached(
        "
        SELECT  c.character_id,
                c.alias,
                c.waypoint,
                c.hardcore,
                c.class,
                b.variant,
                b.body_data,
                c.ethos_good_evil,
                c.ethos_law_chaos,
                c.background,
                c.background_custom_note,
                c.secondary_class,
                c.secondary_class_level,
                c.secondary_class_future_levels,
                c.trigger_slots,
                c.spell_mastery,
                c.pact_standing,
                c.pact_patron_id,
                c.pact_boon,
                c.pact_blade_summoned,
                c.pact_blade_exp,
                c.pact_blade_name,
                c.pact_favour
        FROM    character c
        JOIN    body b ON (c.character_id = b.body_id)
        WHERE   c.player_uuid = ?1
        AND     c.character_id = ?2",
    )?;

    let (body_data, character_data) = stmt.query_row(
        [requesting_player_uuid.clone(), char_id.0.to_string()],
        |row| {
            let character_data = Character {
                character_id: row.get(0)?,
                player_uuid: requesting_player_uuid,
                alias: row.get(1)?,
                waypoint: row.get(2)?,
                hardcore: row.get(3)?,
                class: row.get(4)?,
                ethos_good_evil: row.get(7)?,
                ethos_law_chaos: row.get(8)?,
                background: row.get(9)?,
                background_custom_note: row.get(10)?,
                secondary_class: row.get(11)?,
                secondary_class_level: row.get(12)?,
                secondary_class_future_levels: row.get(13)?,
                trigger_slots: row.get(14)?,
                spell_mastery: row.get(15)?,
                pact_standing: row.get(16)?,
                pact_patron_id: row.get(17)?,
                pact_boon: row.get(18)?,
                pact_blade_summoned: row.get(19)?,
                pact_blade_exp: row.get(20)?,
                pact_blade_name: row.get(21)?,
                pact_favour: row.get(22)?,
            };

            let body_data = Body {
                body_id: row.get(0)?,
                variant: row.get(5)?,
                body_data: row.get(6)?,
            };

            Ok((body_data, character_data))
        },
    )?;

    let (char_waypoint, char_map_marker) =
        convert_waypoint_or_warn(character_data.waypoint.as_deref(), char_id);

    let mut stmt = connection.prepare_cached(
        "
        SELECT  skill_group_kind,
                earned_exp,
                spent_exp,
                skills,
                hash_val,
                direct_earned_sp,
                direct_available_sp
        FROM    skill_group
        WHERE   entity_id = ?1",
    )?;

    let skill_group_data = stmt
        .query_map([char_id.0], |row| {
            Ok(SkillGroup {
                entity_id: char_id.0,
                skill_group_kind: row.get(0)?,
                earned_exp: row.get(1)?,
                spent_exp: row.get(2)?,
                skills: row.get(3)?,
                hash_val: row.get(4)?,
                direct_earned_sp: row.get(5)?,
                direct_available_sp: row.get(6)?,
            })
        })?
        .filter_map(Result::ok)
        .collect::<Vec<SkillGroup>>();

    #[rustfmt::skip]
    let mut stmt = connection.prepare_cached("
        SELECT  p.pet_id,
                p.name,
                b.variant,
                b.body_data
        FROM    pet p
        JOIN    body b ON (p.pet_id = b.body_id)
        WHERE   p.character_id = ?1",
    )?;

    let db_pets = stmt
        .query_map([char_id.0], |row| {
            Ok(Pet {
                database_id: row.get(0)?,
                name: row.get(1)?,
                body_variant: row.get(2)?,
                body_data: row.get(3)?,
            })
        })?
        .filter_map(Result::ok)
        .collect::<Vec<Pet>>();

    // Re-construct the pet components for the player's pets, including
    // de-serializing the pets' bodies and creating their Pet and Stats
    // components
    let pets = db_pets
        .iter()
        .filter_map(|db_pet| {
            if let Ok(pet_body) =
                convert_body_from_database(&db_pet.body_variant, &db_pet.body_data)
            {
                let pet = comp::Pet::new_from_database(
                    NonZeroU64::new(db_pet.database_id as u64).unwrap(),
                );
                let npc_names = NPC_NAMES.read();
                // TODO: use proper name here when pet names will be added
                let pet_stats = comp::Stats::new(
                    npc_names
                        .get_default_name(&pet_body)
                        .unwrap_or(Content::Plain("".to_owned())),
                    pet_body,
                );
                Some((pet, pet_body, pet_stats))
            } else {
                warn!(
                    "Failed to deserialize pet_id: {} for character_id {}",
                    db_pet.database_id, char_id.0
                );
                None
            }
        })
        .collect::<Vec<(comp::Pet, comp::Body, comp::Stats)>>();

    let mut stmt = connection.prepare_cached(
        "
            SELECT  ability_sets
            FROM    ability_set
            WHERE   entity_id = ?1",
    )?;

    let ability_set_data = stmt.query_row([char_id.0], |row| {
        Ok(AbilitySets {
            entity_id: char_id.0,
            ability_sets: row.get(0)?,
        })
    })?;

    let (skill_set, skill_set_persistence_load_error) =
        convert_skill_set_from_database(&skill_group_data);
    let body = convert_body_from_database(&body_data.variant, &body_data.body_data)?;
    let hardcore = convert_hardcore_from_database(character_data.hardcore)?;
    let character_class = convert_class_from_database(
        &character_data.class,
        character_data.secondary_class.as_deref(),
        character_data.secondary_class_level,
        character_data.secondary_class_future_levels,
    );
    // Built before the pool below, because the pool is derived from the
    // spellbook this carries.
    let inventory = convert_inventory_from_database_items(
        character_containers.inventory_container_id,
        &inventory_items,
        character_containers.loadout_container_id,
        &loadout_items,
        character_containers.overflow_items_container_id,
        &overflow_items_items,
        &recipe_book_items,
        &spell_book_items,
    )?;
    // Xindeler: persisted `Innate` hotbar slots name pool keys, so resolving
    // them needs the pool. Nothing here holds one yet — the component is only
    // inserted once the character is applied to its entity — so rebuild it from
    // the body, class, spellbook and pact we just decoded, exactly as
    // `state_ext` will. Both sites must agree, or a slot would resolve to a
    // different ability than the one it was saved against — which is why both
    // go through `for_character_with_pact` rather than `for_character`: a
    // Warlock who logged out with the blade summoned keeps its three attack
    // keys across the relog, and so keeps the hotbar slots bound to them.
    let pact = convert_pact_from_database(
        character_data.pact_standing.as_deref(),
        character_data.pact_patron_id.as_deref(),
        character_data.pact_boon.as_deref(),
        character_data.pact_blade_summoned,
        character_data.pact_blade_exp,
        character_data.pact_blade_name.as_deref(),
        character_data.pact_favour,
    );
    let ability_pool = comp::ability::AbilityPool::for_character_with_pact(
        &body,
        &character_class,
        inventory.learned_spells(),
        Some(&pact),
    );
    Ok((
        PersistedComponents {
            body,
            hardcore,
            character_class,
            stats: convert_stats_from_database(character_data.alias, body),
            skill_set,
            inventory,
            waypoint: char_waypoint,
            pets,
            active_abilities: convert_active_abilities_from_database(
                &ability_set_data,
                &ability_pool,
            ),
            map_marker: char_map_marker,
            ethos: convert_ethos_from_database(
                character_data.ethos_good_evil,
                character_data.ethos_law_chaos,
            ),
            background: convert_background_from_database(character_data.background.as_deref()),
            pact,
            trigger_slots: json_models::db_string_to_trigger_slots(
                character_data.trigger_slots.as_deref(),
                &ability_pool,
            ),
            spell_mastery: json_models::db_string_to_spell_mastery(
                character_data.spell_mastery.as_deref(),
            ),
        },
        UpdateCharacterMetadata {
            skill_set_persistence_load_error,
        },
    ))
}

/// Loads a list of characters belonging to the player. This data is a small
/// subset of the character's data, and is used to render the character and
/// their level in the character list.
///
/// In the event that a join fails, for a character (i.e. they lack an entry for
/// stats, body, etc...) the character is skipped, and no entry will be
/// returned.
pub fn load_character_list(player_uuid_: &str, connection: &Connection) -> CharacterListResult {
    let mut stmt = connection.prepare_cached(
        "
            SELECT  character_id,
                    alias,
                    waypoint,
                    hardcore,
                    class
            FROM    character
            WHERE   player_uuid = ?1
            ORDER BY character_id",
    )?;

    let characters = stmt
        .query_map([player_uuid_], |row| {
            Ok(Character {
                character_id: row.get(0)?,
                alias: row.get(1)?,
                player_uuid: player_uuid_.to_owned(),
                waypoint: row.get(2)?,
                hardcore: row.get(3)?,
                class: row.get(4)?,
                // The character-list view doesn't surface alignment; defaulted.
                ethos_good_evil: 0,
                ethos_law_chaos: 0,
                // Nor background (BL-31): defaulted, same as ethos above.
                background: None,
                background_custom_note: None,
                // Nor the secondary class: defaulted, same reasoning.
                secondary_class: None,
                secondary_class_level: 0,
                secondary_class_future_levels: 0,
                // Nor the trigger slots: the list view never shows them.
                trigger_slots: None,
                // Nor spell mastery: the list view never shows it either.
                spell_mastery: None,
                // Nor the pact: the list view never shows it either.
                pact_standing: None,
                pact_patron_id: None,
                pact_boon: None,
                pact_blade_summoned: None,
                pact_blade_exp: None,
                pact_blade_name: None,
                pact_favour: None,
            })
        })?
        .map(|x| x.unwrap())
        .collect::<Vec<Character>>();
    drop(stmt);

    characters
        .iter()
        .map(|character_data| {
            let char = convert_character_from_database(character_data);

            let mut stmt = connection.prepare_cached(
                "
                SELECT  body_id,
                        variant,
                        body_data
                FROM    body
                WHERE   body_id = ?1",
            )?;
            let db_body = stmt.query_row([char.id.map(|c| c.0)], |row| {
                Ok(Body {
                    body_id: row.get(0)?,
                    variant: row.get(1)?,
                    body_data: row.get(2)?,
                })
            })?;
            drop(stmt);

            let char_body = convert_body_from_database(&db_body.variant, &db_body.body_data)?;

            let hardcore = convert_hardcore_from_database(character_data.hardcore)?;

            let loadout_container_id = get_pseudo_container_id(
                connection,
                CharacterId(character_data.character_id),
                LOADOUT_PSEUDO_CONTAINER_POSITION,
            )?;

            let loadout_items = load_items(connection, loadout_container_id)?;

            let loadout =
                convert_loadout_from_database_items(loadout_container_id, &loadout_items)?;

            let recipe_book_container_id = get_pseudo_container_id(
                connection,
                CharacterId(character_data.character_id),
                RECIPE_BOOK_PSEUDO_CONTAINER_POSITION,
            )?;

            let recipe_book_items = load_items(connection, recipe_book_container_id)?;

            let (recipe_book, _) = convert_recipe_book_from_database_items(&recipe_book_items)?;

            let (char_waypoint, _char_map_marker) = convert_waypoint_or_warn(
                character_data.waypoint.as_deref(),
                CharacterId(character_data.character_id),
            );
            let location = char_waypoint.map(|w| w.get_pos());

            let suspended =
                get_character_suspension(CharacterId(character_data.character_id), connection)?
                    .map(|record| CharacterSuspension {
                        reason: record.reason,
                        end_date: record.end_date.map(|d| d.timestamp()),
                    });

            // Xindeler: the spell book is deliberately NOT loaded here. Nothing
            // on the character-select screen reads it, and looking its
            // pseudo-container up would make the whole character LIST fail for
            // any character the backfill missed — a far worse failure than an
            // absent spellbook on a screen that never shows one.
            Ok(CharacterItem {
                character: char,
                body: char_body,
                hardcore: hardcore.is_some(),
                inventory: Inventory::with_loadout(loadout, char_body)
                    .with_recipe_book(recipe_book),
                location,
                suspended,
            })
        })
        .collect()
}

pub fn create_character(
    uuid: &str,
    character_alias: &str,
    persisted_components: PersistedComponents,
    transaction: &mut Transaction,
) -> CharacterCreationResult {
    check_character_limit(uuid, transaction)?;
    let character_alias = normalize_character_name(character_alias);
    validate_character_name(&character_alias).map_err(PersistenceError::CharacterNameInvalid)?;

    let PersistedComponents {
        body,
        hardcore,
        character_class,
        stats: _,
        skill_set,
        inventory,
        waypoint,
        pets: _,
        active_abilities,
        map_marker,
        ethos,
        background,
        // A brand-new character has nothing configured; the column stays NULL
        // and the first autosave that changes it writes the row.
        trigger_slots: _,
        // Nothing accrued yet either; same reasoning as `trigger_slots`.
        spell_mastery: _,
        // A brand-new character has no pact yet either; the columns stay
        // NULL and load back as `Pact::default()`, same reasoning.
        pact: _,
    } = persisted_components;

    // Fetch new entity IDs for character, inventory, loadout, overflow items,
    // recipe book and spell book
    let mut new_entity_ids = get_new_entity_ids(transaction, |next_id| next_id + 6)?;

    // Create pseudo-container items for character
    let character_id = new_entity_ids.next().unwrap();
    let inventory_container_id = new_entity_ids.next().unwrap();
    let loadout_container_id = new_entity_ids.next().unwrap();
    let overflow_items_container_id = new_entity_ids.next().unwrap();
    let recipe_book_container_id = new_entity_ids.next().unwrap();
    let spell_book_container_id = new_entity_ids.next().unwrap();

    let pseudo_containers = vec![
        Item {
            stack_size: 1,
            item_id: character_id,
            parent_container_item_id: WORLD_PSEUDO_CONTAINER_ID,
            item_definition_id: CHARACTER_PSEUDO_CONTAINER_DEF_ID.to_owned(),
            position: character_id.to_string(),
            properties: String::new(),
        },
        Item {
            stack_size: 1,
            item_id: inventory_container_id,
            parent_container_item_id: character_id,
            item_definition_id: INVENTORY_PSEUDO_CONTAINER_DEF_ID.to_owned(),
            position: INVENTORY_PSEUDO_CONTAINER_POSITION.to_owned(),
            properties: String::new(),
        },
        Item {
            stack_size: 1,
            item_id: loadout_container_id,
            parent_container_item_id: character_id,
            item_definition_id: LOADOUT_PSEUDO_CONTAINER_DEF_ID.to_owned(),
            position: LOADOUT_PSEUDO_CONTAINER_POSITION.to_owned(),
            properties: String::new(),
        },
        Item {
            stack_size: 1,
            item_id: overflow_items_container_id,
            parent_container_item_id: character_id,
            item_definition_id: OVERFLOW_ITEMS_PSEUDO_CONTAINER_DEF_ID.to_owned(),
            position: OVERFLOW_ITEMS_PSEUDO_CONTAINER_POSITION.to_owned(),
            properties: String::new(),
        },
        Item {
            stack_size: 1,
            item_id: recipe_book_container_id,
            parent_container_item_id: character_id,
            item_definition_id: RECIPE_BOOK_PSEUDO_CONTAINER_DEF_ID.to_owned(),
            position: RECIPE_BOOK_PSEUDO_CONTAINER_POSITION.to_owned(),
            properties: String::new(),
        },
        Item {
            stack_size: 1,
            item_id: spell_book_container_id,
            parent_container_item_id: character_id,
            item_definition_id: SPELL_BOOK_PSEUDO_CONTAINER_DEF_ID.to_owned(),
            position: SPELL_BOOK_PSEUDO_CONTAINER_POSITION.to_owned(),
            properties: String::new(),
        },
    ];

    let mut stmt = transaction.prepare_cached(
        "
        INSERT INTO item (item_id,
                          parent_container_item_id,
                          item_definition_id,
                          stack_size,
                          position,
                          properties)
        VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
    )?;

    for pseudo_container in pseudo_containers {
        stmt.execute([
            &pseudo_container.item_id as &dyn ToSql,
            &pseudo_container.parent_container_item_id,
            &pseudo_container.item_definition_id,
            &pseudo_container.stack_size,
            &pseudo_container.position,
            &pseudo_container.properties,
        ])?;
    }
    drop(stmt);

    let mut stmt = transaction.prepare_cached(
        "
        INSERT INTO body (body_id,
                          variant,
                          body_data)
        VALUES (?1, ?2, ?3)",
    )?;

    let (body_variant, body_json) = convert_body_to_database_json(&body)?;
    stmt.execute([
        &character_id as &dyn ToSql,
        &body_variant.to_string(),
        &body_json,
    ])?;
    drop(stmt);

    let (background_db, background_custom_note_db) = convert_background_to_database(background);

    let mut stmt = transaction.prepare_cached(
        "
        INSERT INTO character (character_id,
                               player_uuid,
                               alias,
                               waypoint,
                               hardcore,
                               class,
                               ethos_good_evil,
                               ethos_law_chaos,
                               background,
                               background_custom_note,
                               secondary_class,
                               secondary_class_level,
                               secondary_class_future_levels)
        VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)",
    )?;

    match stmt.execute([
        &character_id as &dyn ToSql,
        &uuid,
        &character_alias,
        &convert_waypoint_to_database_json(waypoint, map_marker),
        &convert_hardcore_to_database(hardcore),
        &convert_class_to_database(character_class),
        &ethos.good_evil,
        &ethos.law_chaos,
        &background_db,
        &background_custom_note_db,
        &convert_secondary_class_to_database(character_class),
        &convert_secondary_class_level_to_database(character_class),
        &convert_future_levels_to_secondary_to_database(character_class),
    ]) {
        Ok(_) => {},
        Err(DbError::SqliteFailure(error, _)) if error.code == ErrorCode::ConstraintViolation => {
            return Err(PersistenceError::CharacterNameUnavailable);
        },
        Err(error) => return Err(error.into()),
    }
    drop(stmt);

    let db_skill_groups =
        convert_skill_groups_to_database(CharacterId(character_id), skill_set.skill_groups());

    let mut stmt = transaction.prepare_cached(
        "
        INSERT INTO skill_group (entity_id,
                                 skill_group_kind,
                                 earned_exp,
                                 spent_exp,
                                 skills,
                                 hash_val,
                                 direct_earned_sp,
                                 direct_available_sp)
        VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
    )?;

    for skill_group in db_skill_groups {
        stmt.execute([
            &character_id as &dyn ToSql,
            &skill_group.skill_group_kind,
            &skill_group.earned_exp,
            &skill_group.spent_exp,
            &skill_group.skills,
            &skill_group.hash_val,
            &skill_group.direct_earned_sp,
            &skill_group.direct_available_sp,
        ])?;
    }
    drop(stmt);

    // Xindeler: character creation runs once, and no `AbilityPool` component
    // exists for a character that has no entity yet, so rebuilding the pool
    // here is both the only option and a negligible cost.
    let ability_sets = convert_active_abilities_to_database(
        CharacterId(character_id),
        &active_abilities,
        &comp::ability::AbilityPool::for_character(
            &body,
            &character_class,
            inventory.learned_spells(),
        ),
    );

    let mut stmt = transaction.prepare_cached(
        "
        INSERT INTO ability_set (entity_id,
                                 ability_sets)
        VALUES (?1, ?2)",
    )?;

    stmt.execute([
        &character_id as &dyn ToSql,
        &ability_sets.ability_sets as &dyn ToSql,
    ])?;
    drop(stmt);

    // Insert default inventory and loadout item records
    let mut inserts = Vec::new();

    get_new_entity_ids(transaction, |mut next_id| {
        let inserts_ = convert_items_to_database_items(
            loadout_container_id,
            &inventory,
            inventory_container_id,
            overflow_items_container_id,
            recipe_book_container_id,
            spell_book_container_id,
            &mut next_id,
        );
        inserts = inserts_;
        next_id
    })?;

    let mut stmt = transaction.prepare_cached(
        "
        INSERT INTO item (item_id,
                          parent_container_item_id,
                          item_definition_id,
                          stack_size,
                          position,
                          properties)
        VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
    )?;

    for item in inserts {
        stmt.execute([
            &item.model.item_id as &dyn ToSql,
            &item.model.parent_container_item_id,
            &item.model.item_definition_id,
            &item.model.stack_size,
            &item.model.position,
            &item.model.properties,
        ])?;
    }
    drop(stmt);

    load_character_list(uuid, transaction).map(|list| (CharacterId(character_id), list))
}

pub fn edit_character(
    editable_components: EditableComponents,
    trusted_change: Option<PermanentChange>,
    transaction: &mut Transaction,
    character_id: CharacterId,
    uuid: &str,
    character_alias: Option<&str>,
) -> CharacterCreationResult {
    let (body,) = editable_components;
    let mut char_list = load_character_list(uuid, transaction);

    if let Ok(char_list) = &mut char_list
        && let Some(char) = char_list
            .iter_mut()
            .find(|c| c.character.id == Some(character_id))
        && let (comp::Body::Humanoid(new), comp::Body::Humanoid(old)) = (body, char.body)
    {
        let allow_change = match trusted_change {
            Some(change) => change.expected_old_body == char.body,
            None => new.species == old.species && new.body_type == old.body_type,
        };
        if !allow_change {
            warn!(
                "Character edit rejected due to failed validation - Character ID: {} Alias: {:?}",
                character_id.0, character_alias
            );
            return Err(PersistenceError::CharacterDataError);
        } else {
            char.body = body;
        }
    }

    let mut stmt = transaction
        .prepare_cached("UPDATE body SET variant = ?1, body_data = ?2 WHERE body_id = ?3")?;

    let (body_variant, body_data) = convert_body_to_database_json(&body)?;
    stmt.execute([
        &body_variant.to_string(),
        &body_data,
        &character_id.0 as &dyn ToSql,
    ])?;
    drop(stmt);

    if let Some(character_alias) = character_alias {
        // NH-79: the in-game character editor is the other write path that
        // sets `character.alias` besides creation and the new rename
        // endpoint -- same normalize/validate/uniqueness handling as those,
        // so a duplicate/malformed name can't sneak in through this route.
        let character_alias = normalize_character_name(character_alias);
        validate_character_name(&character_alias)
            .map_err(PersistenceError::CharacterNameInvalid)?;

        let mut stmt = transaction
            .prepare_cached("UPDATE character SET alias = ?1 WHERE character_id = ?2")?;

        match stmt.execute([&character_alias as &dyn ToSql, &character_id.0]) {
            Ok(_) => {},
            Err(DbError::SqliteFailure(error, _))
                if error.code == ErrorCode::ConstraintViolation =>
            {
                return Err(PersistenceError::CharacterNameUnavailable);
            },
            Err(error) => return Err(error.into()),
        }
        drop(stmt);
    }

    char_list.map(|list| (character_id, list))
}

/// Permanently deletes a character
pub fn delete_character(
    requesting_player_uuid: &str,
    char_id: CharacterId,
    transaction: &mut Transaction,
) -> Result<(), PersistenceError> {
    debug!(?requesting_player_uuid, ?char_id, "Deleting character");

    let mut stmt = transaction.prepare_cached(
        "
        SELECT  COUNT(1)
        FROM    character
        WHERE   character_id = ?1
        AND     player_uuid = ?2",
    )?;

    let result = stmt.query_row([&char_id.0 as &dyn ToSql, &requesting_player_uuid], |row| {
        let y: i64 = row.get(0)?;
        Ok(y)
    })?;
    drop(stmt);

    if result != 1 {
        // The character does not exist, or does not belong to the requesting player so
        // silently drop the request.
        return Ok(());
    }

    // Delete skill groups
    let mut stmt = transaction.prepare_cached(
        "
        DELETE
        FROM    skill_group
        WHERE   entity_id = ?1",
    )?;

    stmt.execute([&char_id.0])?;
    drop(stmt);

    let pet_ids = get_pet_ids(char_id, transaction)?
        .iter()
        .map(|x| Value::from(*x))
        .collect::<Vec<Value>>();
    if !pet_ids.is_empty() {
        delete_pets(transaction, char_id, Rc::new(pet_ids))?;
    }

    // Delete ability sets
    let mut stmt = transaction.prepare_cached(
        "
        DELETE
        FROM    ability_set
        WHERE   entity_id = ?1",
    )?;

    stmt.execute([&char_id.0])?;
    drop(stmt);

    // Delete the cached portrait, if one was ever rendered. `character_id`s
    // are never reused (`get_new_entity_ids` allocates from `sqlite_sequence`),
    // so an orphaned row could not be served to anybody -- but nothing would
    // ever collect it either, and a picture of a character its player deleted
    // is not something to keep lying around. Inside this transaction so that a
    // deletion that rolls back does not take the portrait with it, and ahead of
    // the character row so the order stays correct if this table ever does
    // acquire a foreign key.
    super::portrait::delete_portrait(char_id, transaction)?;

    // Delete character
    let mut stmt = transaction.prepare_cached(
        "
        DELETE
        FROM    character
        WHERE   character_id = ?1",
    )?;

    stmt.execute([&char_id.0])?;
    drop(stmt);

    // Delete body
    let mut stmt = transaction.prepare_cached(
        "
        DELETE
        FROM    body
        WHERE   body_id = ?1",
    )?;

    stmt.execute([&char_id.0])?;
    drop(stmt);

    // Delete all items, recursively walking all containers starting from the
    // "character" pseudo-container that is the root for all items owned by
    // a character.
    let mut stmt = transaction.prepare_cached(
        "
        WITH RECURSIVE
        parents AS (
            SELECT  item_id
            FROM    item
            WHERE   item.item_id = ?1 -- Item with character id is the character pseudo-container
            UNION ALL
            SELECT  item.item_id
            FROM    item,
                    parents
            WHERE   item.parent_container_item_id = parents.item_id
        )
        DELETE
        FROM    item
        WHERE   EXISTS (SELECT 1 FROM parents WHERE parents.item_id = item.item_id)",
    )?;

    let deleted_item_count = stmt.execute([&char_id.0])?;
    drop(stmt);

    if deleted_item_count < 3 {
        return Err(PersistenceError::OtherError(format!(
            "Error deleting from item table for char_id {} (expected at least 3 deletions, found \
             {})",
            char_id.0, deleted_item_count
        )));
    }

    Ok(())
}

/// Sums `skill_group.earned_exp` for `char_id` and derives the character's
/// overall level from it, without paying for a full character load
/// (body/inventory/pets/etc.) the way `load_character_data` does.
pub fn character_level(
    char_id: CharacterId,
    connection: &Connection,
) -> Result<u16, PersistenceError> {
    let mut stmt = connection.prepare_cached(
        "SELECT COALESCE(SUM(earned_exp), 0) FROM skill_group WHERE entity_id = ?1",
    )?;
    let total_exp: i64 = stmt.query_row([&char_id.0], |row| row.get(0))?;
    drop(stmt);

    // Same i64 -> u32 clamp `conversions::convert_skill_groups_to_database`
    // already applies per-row; this just sums first.
    Ok(level_from_total_exp(
        total_exp.clamp(0, i64::from(u32::MAX)) as u32,
    ))
}

/// A uuid-only sibling of `load_character_list` for NH-79's
/// character-summary endpoint -- that one eagerly loads body/inventory/pets/
/// etc. for the character-select screen, none of which a summary list needs.
/// `CharacterSummary` itself lives one level up, in `persistence::mod`, since
/// this module is `pub(in crate::persistence)` and `Server::
/// list_player_characters` (outside that scope, in `server::lib`) needs to
/// name the return type. Location resolution (waypoint ->
/// site/kingdom/continent) also happens up there, since it needs
/// `Server::parse_locations`' world/index access that persistence doesn't
/// have.
pub fn load_character_summaries(
    player_uuid_: &str,
    connection: &Connection,
) -> Result<Vec<super::CharacterSummary>, PersistenceError> {
    let mut stmt = connection.prepare_cached(
        "
            SELECT  character_id,
                    alias,
                    class,
                    waypoint
            FROM    character
            WHERE   player_uuid = ?1
            ORDER BY character_id",
    )?;

    let rows = stmt
        .query_map([player_uuid_], |row| {
            let character_id: i64 = row.get(0)?;
            let alias: String = row.get(1)?;
            let class: String = row.get(2)?;
            let waypoint: Option<String> = row.get(3)?;
            Ok((character_id, alias, class, waypoint))
        })?
        .collect::<Result<Vec<_>, _>>()?;
    drop(stmt);

    rows.into_iter()
        .map(|(character_id, alias, class, waypoint)| {
            let character_id = CharacterId(character_id);
            let level = character_level(character_id, connection)?;
            let suspended = get_character_suspension(character_id, connection)?;
            Ok(super::CharacterSummary {
                character_id,
                alias,
                class,
                level,
                waypoint,
                suspended,
            })
        })
        .collect()
}

/// Renames one of `requesting_player_uuid`'s own characters (NH-79). Rejects
/// with `CharacterNotFound` if `char_id` doesn't exist or belongs to someone
/// else -- same ownership-check shape as `delete_character`, except a rename
/// has a real client-facing outcome that must not look like success, so this
/// returns an explicit `Err` instead of silently no-op'ing.
pub fn rename_character(
    requesting_player_uuid: &str,
    char_id: CharacterId,
    new_alias: &str,
    transaction: &mut Transaction,
) -> Result<(), PersistenceError> {
    let new_alias = normalize_character_name(new_alias);
    validate_character_name(&new_alias).map_err(PersistenceError::CharacterNameInvalid)?;

    let mut stmt = transaction.prepare_cached(
        "
        SELECT  COUNT(1)
        FROM    character
        WHERE   character_id = ?1
        AND     player_uuid = ?2",
    )?;

    let owned = stmt.query_row([&char_id.0 as &dyn ToSql, &requesting_player_uuid], |row| {
        let count: i64 = row.get(0)?;
        Ok(count)
    })?;
    drop(stmt);

    if owned != 1 {
        return Err(PersistenceError::CharacterNotFound);
    }

    let mut stmt =
        transaction.prepare_cached("UPDATE character SET alias = ?1 WHERE character_id = ?2")?;

    match stmt.execute([&new_alias as &dyn ToSql, &char_id.0]) {
        Ok(_) => {},
        Err(DbError::SqliteFailure(error, _)) if error.code == ErrorCode::ConstraintViolation => {
            return Err(PersistenceError::CharacterNameUnavailable);
        },
        Err(error) => return Err(error.into()),
    }
    drop(stmt);

    Ok(())
}

/// Renders a `DateTime<Utc>` the way every `character_suspensions` timestamp
/// column is stored: RFC3339, always round-trippable through
/// `parse_suspension_timestamp` below.
fn format_suspension_timestamp(at: DateTime<Utc>) -> String { at.to_rfc3339() }

/// The inverse of `format_suspension_timestamp`. A parse failure here means
/// the column holds something that didn't come from this module -- treated as
/// a database error rather than silently defaulting to some placeholder time,
/// since either a suspension's start or its expiry being wrong is exactly the
/// kind of silent bug this feature exists to avoid.
fn parse_suspension_timestamp(raw: &str) -> Result<DateTime<Utc>, PersistenceError> {
    DateTime::parse_from_rfc3339(raw)
        .map(|dt| dt.with_timezone(&Utc))
        .map_err(|err| {
            PersistenceError::OtherError(format!(
                "invalid character_suspensions timestamp {raw:?}: {err}"
            ))
        })
}

/// Suspends `character_id`, replacing any existing suspension on it (`REPLACE
/// INTO` on the single-column primary key -- a re-suspend overwrites rather
/// than stacking, matching the schema's "one row per currently-suspended
/// character" cardinality).
pub fn suspend_character(
    character_id: CharacterId,
    reason: &str,
    suspended_by_operator_uuid: &str,
    suspended_at: DateTime<Utc>,
    end_date: Option<DateTime<Utc>>,
    transaction: &mut Transaction,
) -> Result<(), PersistenceError> {
    let mut stmt = transaction.prepare_cached(
        "
        REPLACE
        INTO    character_suspensions (character_id,
                                        reason,
                                        suspended_by_operator_uuid,
                                        suspended_at,
                                        end_date)
        VALUES  (?1, ?2, ?3, ?4, ?5)",
    )?;
    stmt.execute([
        &character_id.0 as &dyn ToSql,
        &reason,
        &suspended_by_operator_uuid,
        &format_suspension_timestamp(suspended_at),
        &end_date.map(format_suspension_timestamp),
    ])?;
    Ok(())
}

/// Lifts any suspension on `character_id`. Idempotent: deleting a row that
/// isn't there is not an error.
pub fn unsuspend_character(
    character_id: CharacterId,
    transaction: &mut Transaction,
) -> Result<(), PersistenceError> {
    let mut stmt =
        transaction.prepare_cached("DELETE FROM character_suspensions WHERE character_id = ?1")?;
    stmt.execute([&character_id.0])?;
    Ok(())
}

/// The current suspension row for `character_id`, if any.
pub fn get_character_suspension(
    character_id: CharacterId,
    connection: &Connection,
) -> Result<Option<super::SuspensionRecord>, PersistenceError> {
    let mut stmt = connection.prepare_cached(
        "
        SELECT  reason,
                suspended_by_operator_uuid,
                suspended_at,
                end_date
        FROM    character_suspensions
        WHERE   character_id = ?1",
    )?;
    let row = stmt.query_row([character_id.0], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, String>(2)?,
            row.get::<_, Option<String>>(3)?,
        ))
    });
    match row {
        Ok((reason, suspended_by_operator_uuid, suspended_at, end_date)) => {
            Ok(Some(super::SuspensionRecord {
                reason,
                suspended_by_operator_uuid,
                suspended_at: parse_suspension_timestamp(&suspended_at)?,
                end_date: end_date
                    .as_deref()
                    .map(parse_suspension_timestamp)
                    .transpose()?,
            }))
        },
        Err(DbError::QueryReturnedNoRows) => Ok(None),
        Err(error) => Err(error.into()),
    }
}

/// Every currently-suspended character, for `CharacterSuspensions::load` at
/// server startup.
pub fn load_all_character_suspensions(
    connection: &Connection,
) -> Result<HashMap<CharacterId, super::SuspensionRecord>, PersistenceError> {
    let mut stmt = connection.prepare_cached(
        "
        SELECT  character_id,
                reason,
                suspended_by_operator_uuid,
                suspended_at,
                end_date
        FROM    character_suspensions",
    )?;
    let rows = stmt
        .query_map([], |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, Option<String>>(4)?,
            ))
        })?
        .collect::<Result<Vec<_>, _>>()?;

    rows.into_iter()
        .map(
            |(character_id, reason, suspended_by_operator_uuid, suspended_at, end_date)| {
                Ok((CharacterId(character_id), super::SuspensionRecord {
                    reason,
                    suspended_by_operator_uuid,
                    suspended_at: parse_suspension_timestamp(&suspended_at)?,
                    end_date: end_date
                        .as_deref()
                        .map(parse_suspension_timestamp)
                        .transpose()?,
                }))
            },
        )
        .collect()
}

/// Resolves the account uuid that owns `character_id`, or `None` if no such
/// character exists.
pub fn character_owner_uuid(
    character_id: CharacterId,
    connection: &Connection,
) -> Result<Option<String>, PersistenceError> {
    let mut stmt =
        connection.prepare_cached("SELECT player_uuid FROM character WHERE character_id = ?1")?;
    match stmt.query_row([character_id.0], |row| row.get::<_, String>(0)) {
        Ok(uuid) => Ok(Some(uuid)),
        Err(DbError::QueryReturnedNoRows) => Ok(None),
        Err(error) => Err(error.into()),
    }
}

/// Before creating a character, we ensure that the limit on the number of
/// characters has not been exceeded
pub fn check_character_limit(
    uuid: &str,
    transaction: &mut Transaction,
) -> Result<(), PersistenceError> {
    let mut stmt = transaction.prepare_cached(
        "
        SELECT  COUNT(1)
        FROM    character
        WHERE   player_uuid = ?1",
    )?;

    #[expect(clippy::needless_question_mark)]
    let character_count: i64 = stmt.query_row([&uuid], |row| Ok(row.get(0)?))?;
    drop(stmt);

    if character_count < MAX_CHARACTERS_PER_PLAYER as i64 {
        Ok(())
    } else {
        Err(PersistenceError::CharacterLimitReached)
    }
}

/// NOTE: This relies heavily on serializability to work correctly.
///
/// The count function takes the starting entity id, and returns the desired
/// count of new entity IDs.
///
/// These are then inserted into the entities table.
fn get_new_entity_ids(
    transaction: &mut Transaction,
    mut max: impl FnMut(i64) -> i64,
) -> Result<Range<EntityId>, PersistenceError> {
    // The sqlite_sequence table is used here to avoid reusing entity IDs for
    // deleted entities. This table always contains the highest used ID for
    // each AUTOINCREMENT column in a SQLite database.
    let mut stmt = transaction.prepare_cached(
        "
        SELECT  seq + 1 AS entity_id
        FROM    sqlite_sequence
        WHERE   name = 'entity'",
    )?;

    #[expect(clippy::needless_question_mark)]
    let next_entity_id = stmt.query_row([], |row| Ok(row.get(0)?))?;
    let max_entity_id = max(next_entity_id);

    // Create a new range of IDs and insert them into the entity table
    let new_ids: Range<EntityId> = next_entity_id..max_entity_id;

    let mut stmt = transaction.prepare_cached("INSERT INTO entity (entity_id) VALUES (?1)")?;

    // SQLite has no bulk insert
    for i in new_ids.clone() {
        stmt.execute([i])?;
    }

    trace!(
        "Created {} new persistence entity_ids: {}",
        new_ids.end - new_ids.start,
        new_ids
            .clone()
            .map(|x| x.to_string())
            .collect::<Vec<String>>()
            .join(", ")
    );
    Ok(new_ids)
}

/// Fetches the pseudo_container IDs for a character
fn get_pseudo_containers(
    connection: &Connection,
    character_id: CharacterId,
) -> Result<CharacterContainers, PersistenceError> {
    let character_containers = CharacterContainers {
        loadout_container_id: get_pseudo_container_id(
            connection,
            character_id,
            LOADOUT_PSEUDO_CONTAINER_POSITION,
        )?,
        inventory_container_id: get_pseudo_container_id(
            connection,
            character_id,
            INVENTORY_PSEUDO_CONTAINER_POSITION,
        )?,
        overflow_items_container_id: get_pseudo_container_id(
            connection,
            character_id,
            OVERFLOW_ITEMS_PSEUDO_CONTAINER_POSITION,
        )?,
        recipe_book_container_id: get_pseudo_container_id(
            connection,
            character_id,
            RECIPE_BOOK_PSEUDO_CONTAINER_POSITION,
        )?,
        spell_book_container_id: get_pseudo_container_id(
            connection,
            character_id,
            SPELL_BOOK_PSEUDO_CONTAINER_POSITION,
        )?,
    };

    Ok(character_containers)
}

/// `pub(in crate::persistence)` so that `persistence::portrait` can resolve
/// just the loadout container, rather than paying `get_pseudo_containers` for
/// all five when it needs one.
pub(in crate::persistence) fn get_pseudo_container_id(
    connection: &Connection,
    character_id: CharacterId,
    pseudo_container_position: &str,
) -> Result<EntityId, PersistenceError> {
    let mut stmt = connection.prepare_cached(
        "
        SELECT  item_id
        FROM    item
        WHERE   parent_container_item_id = ?1
        AND     position = ?2",
    )?;

    #[expect(clippy::needless_question_mark)]
    let res = stmt.query_row(
        [
            character_id.0.to_string(),
            pseudo_container_position.to_string(),
        ],
        |row| Ok(row.get(0)?),
    );

    match res {
        Ok(id) => Ok(id),
        Err(e) => {
            error!(
                ?e,
                ?character_id,
                ?pseudo_container_position,
                "Failed to retrieve pseudo container ID"
            );
            Err(DatabaseError(e))
        },
    }
}

/// Stores new pets in the database, and removes pets from the database that the
/// player no longer has. Currently there are no actual updates to pet data
/// since we don't store any updatable data about pets in the database.
fn update_pets(
    char_id: CharacterId,
    pets: Vec<PetPersistenceData>,
    transaction: &mut Transaction,
) -> Result<(), PersistenceError> {
    debug!("Updating {} pets for character {}", pets.len(), char_id.0);

    let db_pets = get_pet_ids(char_id, transaction)?;
    if !db_pets.is_empty() {
        let dead_pet_ids = Rc::new(
            db_pets
                .iter()
                .filter(|pet_id| {
                    !pets.iter().any(|(pet, _, _)| {
                        pet.get_database_id()
                            .load()
                            .is_some_and(|x| x.get() == **pet_id as u64)
                    })
                })
                .map(|x| Value::from(*x))
                .collect::<Vec<Value>>(),
        );

        if !dead_pet_ids.is_empty() {
            delete_pets(transaction, char_id, dead_pet_ids)?;
        }
    }

    for (pet, body, _stats) in pets
        .iter()
        .filter(|(pet, _, _)| pet.get_database_id().load().is_none())
    {
        let pet_entity_id = get_new_entity_ids(transaction, |next_id| next_id + 1)?.start;

        let (body_variant, body_json) = convert_body_to_database_json(body)?;

        #[rustfmt::skip]
        let mut stmt = transaction.prepare_cached("
            INSERT
            INTO    body (
                    body_id,
                    variant,
                    body_data)
            VALUES  (?1, ?2, ?3)"
        )?;

        stmt.execute([
            &pet_entity_id as &dyn ToSql,
            &body_variant.to_string(),
            &body_json,
        ])?;

        #[rustfmt::skip]
        let mut stmt = transaction.prepare_cached("
            INSERT
            INTO    pet (
                    pet_id,
                    character_id,
                    name)
            VALUES  (?1, ?2, ?3)",
        )?;

        // TODO: use pet names here, when such feature will be implemented
        let pet_name = "";
        stmt.execute([&pet_entity_id as &dyn ToSql, &char_id.0, &pet_name])?;
        drop(stmt);

        pet.get_database_id()
            .store(NonZeroU64::new(pet_entity_id as u64));
    }

    Ok(())
}

fn get_pet_ids(
    char_id: CharacterId,
    transaction: &mut Transaction,
) -> Result<Vec<i64>, PersistenceError> {
    #[rustfmt::skip]
        let mut stmt = transaction.prepare_cached("
        SELECT  pet_id
        FROM    pet
        WHERE   character_id = ?1
    ")?;

    #[expect(clippy::needless_question_mark)]
    let db_pets = stmt
        .query_map([&char_id.0], |row| Ok(row.get(0)?))?
        .map(|x| x.unwrap())
        .collect::<Vec<i64>>();
    drop(stmt);
    Ok(db_pets)
}

fn delete_pets(
    transaction: &mut Transaction,
    char_id: CharacterId,
    pet_ids: Rc<Vec<Value>>,
) -> Result<(), PersistenceError> {
    #[rustfmt::skip]
    let mut stmt = transaction.prepare_cached("
            DELETE
            FROM    pet
            WHERE   pet_id IN rarray(?1)"
    )?;

    let delete_count = stmt.execute([&pet_ids])?;
    drop(stmt);
    debug!(
        "Deleted {} pets for character id {}",
        delete_count, char_id.0
    );

    #[rustfmt::skip]
    let mut stmt = transaction.prepare_cached("
            DELETE
            FROM    body
            WHERE   body_id IN rarray(?1)"
    )?;

    let delete_count = stmt.execute([&pet_ids])?;
    debug!(
        "Deleted {} pet bodies for character id {}",
        delete_count, char_id.0
    );

    Ok(())
}

pub fn update(
    char_id: CharacterId,
    char_skill_set: comp::SkillSet,
    inventory: Inventory,
    pets: Vec<PetPersistenceData>,
    char_waypoint: Option<comp::Waypoint>,
    active_abilities: comp::ability::ActiveAbilities,
    // Xindeler: the character's live pool, needed to persist `Innate` hotbar
    // slots by key. Passed in rather than rebuilt: this runs on every save
    // batch, the caller already has the component in its ECS storage, and
    // rebuilding would also need a `Body` this function is not given.
    ability_pool: comp::ability::AbilityPool,
    map_marker: Option<comp::MapMarker>,
    character_class: comp::CharacterClass,
    ethos: comp::Ethos,
    background: comp::Background,
    pact: comp::Pact,
    trigger_slots: comp::TriggerSlots,
    spell_mastery: comp::SpellMastery,
    transaction: &mut Transaction,
) -> Result<(), PersistenceError> {
    // Run pet persistence
    update_pets(char_id, pets, transaction)?;

    let pseudo_containers = get_pseudo_containers(transaction, char_id)?;
    let mut upserts = Vec::new();
    // First, get all the entity IDs for any new items, and identify which
    // slots to upsert and which ones to delete.
    get_new_entity_ids(transaction, |mut next_id| {
        let upserts_ = convert_items_to_database_items(
            pseudo_containers.loadout_container_id,
            &inventory,
            pseudo_containers.inventory_container_id,
            pseudo_containers.overflow_items_container_id,
            pseudo_containers.recipe_book_container_id,
            pseudo_containers.spell_book_container_id,
            &mut next_id,
        );
        upserts = upserts_;
        next_id
    })?;

    // Next, delete any slots we aren't upserting.
    trace!("Deleting items for character_id {}", char_id.0);
    let mut existing_item_ids: Vec<_> = vec![
        Value::from(pseudo_containers.inventory_container_id),
        Value::from(pseudo_containers.loadout_container_id),
        Value::from(pseudo_containers.overflow_items_container_id),
        Value::from(pseudo_containers.recipe_book_container_id),
        Value::from(pseudo_containers.spell_book_container_id),
    ];
    for it in load_items(transaction, pseudo_containers.inventory_container_id)? {
        existing_item_ids.push(Value::from(it.item_id));
    }
    for it in load_items(transaction, pseudo_containers.loadout_container_id)? {
        existing_item_ids.push(Value::from(it.item_id));
    }
    for it in load_items(transaction, pseudo_containers.overflow_items_container_id)? {
        existing_item_ids.push(Value::from(it.item_id));
    }
    for it in load_items(transaction, pseudo_containers.recipe_book_container_id)? {
        existing_item_ids.push(Value::from(it.item_id));
    }
    for it in load_items(transaction, pseudo_containers.spell_book_container_id)? {
        existing_item_ids.push(Value::from(it.item_id));
    }

    let non_upserted_items = upserts
        .iter()
        .map(|item_pair| Value::from(item_pair.model.item_id))
        .collect::<Vec<Value>>();

    let mut stmt = transaction.prepare_cached(
        "
        DELETE
        FROM    item
        WHERE   parent_container_item_id
        IN      rarray(?1)
        AND     item_id NOT IN rarray(?2)",
    )?;
    let delete_count = stmt.execute([Rc::new(existing_item_ids), Rc::new(non_upserted_items)])?;
    trace!("Deleted {} items", delete_count);

    // Upsert items
    let expected_upsert_count = upserts.len();
    if expected_upsert_count > 0 {
        let (upserted_items, _): (Vec<_>, Vec<_>) = upserts
            .into_iter()
            .map(|model_pair| {
                debug_assert_eq!(
                    model_pair.model.item_id,
                    model_pair.comp.load().unwrap().get() as i64
                );
                (model_pair.model, model_pair.comp)
            })
            .unzip();
        trace!(
            "Upserting items {:?} for character_id {}",
            upserted_items, char_id.0
        );

        // When moving inventory items around, foreign key constraints on
        // `parent_container_item_id` can be temporarily violated by one
        // upsert, but restored by another upsert. Deferred constraints
        // allow SQLite to check this when committing the transaction.
        // The `defer_foreign_keys` pragma treats the foreign key
        // constraints as deferred for the next transaction (it turns itself
        // off at the commit boundary). https://sqlite.org/foreignkeys.html#fk_deferred
        transaction.pragma_update(None, "defer_foreign_keys", "ON")?;

        let mut stmt = transaction.prepare_cached(
            "
            REPLACE
            INTO    item (item_id,
                          parent_container_item_id,
                          item_definition_id,
                          stack_size,
                          position,
                          properties)
            VALUES  (?1, ?2, ?3, ?4, ?5, ?6)",
        )?;

        for item in upserted_items.iter() {
            stmt.execute([
                &item.item_id as &dyn ToSql,
                &item.parent_container_item_id,
                &item.item_definition_id,
                &item.stack_size,
                &item.position,
                &item.properties,
            ])?;
        }
    }

    let db_skill_groups = convert_skill_groups_to_database(char_id, char_skill_set.skill_groups());

    let mut stmt = transaction.prepare_cached(
        "
        REPLACE
        INTO    skill_group (entity_id,
                             skill_group_kind,
                             earned_exp,
                             spent_exp,
                             skills,
                             hash_val,
                             direct_earned_sp,
                             direct_available_sp)
        VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
    )?;

    for skill_group in db_skill_groups {
        stmt.execute([
            &skill_group.entity_id as &dyn ToSql,
            &skill_group.skill_group_kind,
            &skill_group.earned_exp,
            &skill_group.spent_exp,
            &skill_group.skills,
            &skill_group.hash_val,
            &skill_group.direct_earned_sp,
            &skill_group.direct_available_sp,
        ])?;
    }

    let db_waypoint = convert_waypoint_to_database_json(char_waypoint, map_marker);
    let (background_db, background_custom_note_db) = convert_background_to_database(background);
    let (
        pact_standing_db,
        pact_patron_db,
        pact_boon_db,
        pact_blade_summoned_db,
        pact_blade_exp_db,
        pact_blade_name_db,
        pact_favour_db,
    ) = convert_pact_to_database(pact);

    let mut stmt = transaction.prepare_cached(
        "
        UPDATE  character
        SET     waypoint = ?1,
                class = ?2,
                ethos_good_evil = ?3,
                ethos_law_chaos = ?4,
                background = ?5,
                background_custom_note = ?6,
                secondary_class = ?7,
                secondary_class_level = ?8,
                secondary_class_future_levels = ?9,
                trigger_slots = ?10,
                spell_mastery = ?11,
                pact_standing = ?12,
                pact_patron_id = ?13,
                pact_boon = ?14,
                pact_blade_summoned = ?15,
                pact_blade_exp = ?16,
                pact_blade_name = ?17,
                pact_favour = ?18
        WHERE   character_id = ?19
    ",
    )?;

    let waypoint_count = stmt.execute([
        &db_waypoint as &dyn ToSql,
        &convert_class_to_database(character_class),
        &ethos.good_evil,
        &ethos.law_chaos,
        &background_db,
        &background_custom_note_db,
        &convert_secondary_class_to_database(character_class),
        &convert_secondary_class_level_to_database(character_class),
        &convert_future_levels_to_secondary_to_database(character_class),
        &json_models::trigger_slots_to_db_string(&trigger_slots, &ability_pool),
        &json_models::spell_mastery_to_db_string(&spell_mastery),
        &pact_standing_db,
        &pact_patron_db,
        &pact_boon_db,
        &pact_blade_summoned_db,
        &pact_blade_exp_db,
        &pact_blade_name_db,
        &pact_favour_db,
        &char_id.0,
    ])?;

    if waypoint_count != 1 {
        return Err(PersistenceError::OtherError(format!(
            "Error updating character table for char_id {}",
            char_id.0
        )));
    }

    let ability_sets =
        convert_active_abilities_to_database(char_id, &active_abilities, &ability_pool);

    let mut stmt = transaction.prepare_cached(
        "
        UPDATE  ability_set
        SET     ability_sets = ?1
        WHERE   entity_id = ?2
    ",
    )?;

    let ability_sets_count = stmt.execute([
        &ability_sets.ability_sets as &dyn ToSql,
        &char_id.0 as &dyn ToSql,
    ])?;

    if ability_sets_count != 1 {
        return Err(PersistenceError::OtherError(format!(
            "Error updating ability_set table for char_id {}",
            char_id.0,
        )));
    }

    Ok(())
}

/// Xindeler: end-to-end persistence tests for the spell book.
///
/// These run the REAL migration set against a fresh SQLite file and then drive
/// `create_character` / `update` / `load_character_data`, because the whole
/// point of the exercise is the interaction between the schema, the
/// pseudo-container backfill, and the entity-id sequence — none of which a unit
/// test over the conversion functions alone would exercise.
#[cfg(test)]
mod spell_book_persistence_tests {
    use super::*;
    use crate::persistence::{ConnectionMode, DatabaseSettings, SqlLogMode, establish_connection};
    use common::comp::{
        ActiveAbilities, CharacterClass, ability::AuxiliaryAbility, item::ItemKind,
    };

    const SPELL_BOOK: &str = "common.items.spell_books.wanderers_primer";
    const SECOND_SPELL_BOOK: &str = "common.items.spell_books.scavenged_page";

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

        fn connection(&self) -> crate::persistence::VelorenConnection {
            establish_connection(&self.settings, ConnectionMode::ReadWrite)
        }
    }

    fn spell_group(specifier: &str) -> common::comp::Item {
        let item = common::comp::Item::new_from_asset_expect(specifier);
        assert!(
            matches!(&*item.kind(), ItemKind::SpellGroup { .. }),
            "{specifier} must be a SpellGroup for these tests to mean anything"
        );
        item
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

    /// Creates a character holding `books` in its spellbook and returns its id.
    fn create(db: &TestDb, uuid: &str, books: &[&str]) -> CharacterId {
        let mut inventory = Inventory::with_empty();
        for specifier in books {
            inventory
                .push_spell_group(spell_group(specifier))
                .expect("distinct groups are accepted");
        }
        let mut conn = db.connection();
        let mut transaction = conn.connection.transaction().expect("transaction");
        let (id, _) =
            create_character(uuid, "Testificate", components(inventory), &mut transaction)
                .expect("character creation");
        transaction.commit().expect("commit");
        id
    }

    fn load(db: &TestDb, uuid: &str, id: CharacterId) -> PersistedComponents {
        let conn = db.connection();
        load_character_data(uuid.to_owned(), id, &conn)
            .expect("character load")
            .0
    }

    /// Saves the character back with whatever inventory/abilities it now has,
    /// the way the persistence tick would.
    fn save(db: &TestDb, id: CharacterId, loaded: &PersistedComponents, pool: &comp::AbilityPool) {
        let mut conn = db.connection();
        let mut transaction = conn.connection.transaction().expect("transaction");
        update(
            id,
            loaded.skill_set.clone(),
            loaded.inventory.clone(),
            Vec::new(),
            loaded.waypoint,
            loaded.active_abilities.clone(),
            pool.clone(),
            loaded.map_marker,
            loaded.character_class,
            loaded.ethos,
            loaded.background,
            loaded.pact.clone(),
            loaded.trigger_slots.clone(),
            loaded.spell_mastery,
            &mut transaction,
        )
        .expect("character update");
        transaction.commit().expect("commit");
    }

    fn pool_of(loaded: &PersistedComponents) -> comp::AbilityPool {
        comp::ability::AbilityPool::for_character_with_pact(
            &loaded.body,
            &loaded.character_class,
            loaded.inventory.learned_spells(),
            Some(&loaded.pact),
        )
    }

    /// (a) the spellbook survives a save/load round-trip.
    #[test]
    fn a_non_empty_spell_book_survives_the_round_trip() {
        let db = TestDb::new();
        let id = create(&db, "uuid-survives", &[SPELL_BOOK]);

        let loaded = load(&db, "uuid-survives", id);
        let expected = spell_group(SPELL_BOOK);
        let ItemKind::SpellGroup { spells } = &*expected.kind() else {
            unreachable!()
        };
        assert_eq!(loaded.inventory.spell_book_len(), spells.len());
        for key in spells {
            assert!(
                loaded.inventory.spell_is_known(key),
                "{key} did not survive the round trip"
            );
        }
        assert_eq!(loaded.inventory.spell_groups_iter().count(), 1);

        // And again after an explicit save, which is the path a live server
        // actually takes.
        let pool = pool_of(&loaded);
        save(&db, id, &loaded, &pool);
        let reloaded = load(&db, "uuid-survives", id);
        for key in spells {
            assert!(reloaded.inventory.spell_is_known(key));
        }
    }

    /// (b) the `AbilityPool` derived from the reloaded character is identical
    /// to the one derived before the save — same keys, same order, same gates.
    #[test]
    fn the_ability_pool_ordering_is_identical_across_the_round_trip() {
        let db = TestDb::new();
        let id = create(&db, "uuid-ordering", &[SPELL_BOOK]);

        let first = load(&db, "uuid-ordering", id);
        let before = pool_of(&first);
        save(&db, id, &first, &before);

        let second = load(&db, "uuid-ordering", id);
        let after = pool_of(&second);

        assert_eq!(before.abilities, after.abilities);
        assert_eq!(before.spell_gates, after.spell_gates);
        // The learned keys really are in there, at the very end.
        let book = spell_group(SPELL_BOOK);
        let ItemKind::SpellGroup { spells } = &*book.kind() else {
            unreachable!()
        };
        let tail = &after.abilities[after.abilities.len() - spells.len()..];
        let mut expected: Vec<&String> = spells.iter().collect();
        expected.sort();
        assert_eq!(tail.iter().collect::<Vec<_>>(), expected);
    }

    /// (c) a persisted hotbar slot bound to a learned spell still resolves to
    /// the SAME ability after a reload.
    #[test]
    fn a_bound_hotbar_slot_still_names_the_same_ability_after_reload() {
        let db = TestDb::new();
        let id = create(&db, "uuid-hotbar", &[SPELL_BOOK]);

        let mut loaded = load(&db, "uuid-hotbar", id);
        let pool = pool_of(&loaded);
        // Bind the last pool entry: a learned spell, the most fragile case.
        let index = pool.abilities.len() - 1;
        let bound_key = pool.abilities[index].clone();
        assert!(
            loaded.inventory.spell_is_known(&bound_key),
            "the last entry should be a learned spell"
        );
        let mut sets = hashbrown::HashMap::new();
        sets.insert((None, None), vec![AuxiliaryAbility::Innate(index)]);
        loaded.active_abilities = ActiveAbilities::from_auxiliary(sets, None);

        save(&db, id, &loaded, &pool);

        let reloaded = load(&db, "uuid-hotbar", id);
        let reloaded_pool = pool_of(&reloaded);
        let slot = reloaded
            .active_abilities
            .auxiliary_sets
            .get(&(None, None))
            .and_then(|set| set.first())
            .copied()
            .expect("the bound set survives");
        let AuxiliaryAbility::Innate(reloaded_index) = slot else {
            panic!("the slot must still be an innate binding, got {slot:?}")
        };
        assert_eq!(
            reloaded_pool.abilities[reloaded_index], bound_key,
            "the hotbar slot re-pointed at a different ability across a reload"
        );
    }

    /// N27-AB: a Warlock who logs out with the pact blade summoned gets it
    /// back on login, and a hotbar slot bound to one of its three attacks
    /// still names the SAME attack.
    ///
    /// This is the load-side half of the "both builders must agree" rule:
    /// `load_character_data` resolves the persisted `Innate:key:` slots
    /// against a pool it rebuilds itself, and `state_ext` builds the live
    /// component separately. If either one used `for_character` instead of
    /// `for_character_with_pact`, the blade keys would be missing from that
    /// side and the binding would silently empty.
    #[test]
    fn a_summoned_blade_and_its_hotbar_slot_survive_a_relog() {
        use common::comp::{
            ability::AbilityPool,
            pact::{PactBoon, PactStanding},
        };

        let db = TestDb::new();
        let id = create(&db, "uuid-blade", &[]);

        // A brand-new character has no pact (`create_character` skips those
        // columns), so bind the blade on the first save instead.
        let mut loaded = load(&db, "uuid-blade", id);
        loaded.character_class = CharacterClass::single(comp::ClassKind::Warlock);
        loaded.pact = comp::Pact {
            standing: PactStanding::Bound,
            boon: Some(PactBoon::Blade),
            blade_summoned: true,
            ..comp::Pact::default()
        };
        let pool = pool_of(&loaded);
        assert!(
            pool.has_blade_bond(),
            "a summoned blade must put its keys in the pool"
        );

        // Bind the capstone: the LAST of the three appended keys, the most
        // fragile position.
        let bound_key = AbilityPool::PACT_BLADE_KEYS[2];
        let index = pool
            .abilities
            .iter()
            .position(|key| key == bound_key)
            .expect("the capstone key is in the pool");
        let mut sets = hashbrown::HashMap::new();
        sets.insert((None, None), vec![AuxiliaryAbility::Innate(index)]);
        loaded.active_abilities = ActiveAbilities::from_auxiliary(sets, None);

        save(&db, id, &loaded, &pool);

        let reloaded = load(&db, "uuid-blade", id);
        assert!(
            reloaded.pact.blade_is_manifest(),
            "blade_summoned must survive the round trip"
        );
        let reloaded_pool = pool_of(&reloaded);
        assert!(reloaded_pool.has_blade_bond());
        let slot = reloaded
            .active_abilities
            .auxiliary_sets
            .get(&(None, None))
            .and_then(|set| set.first())
            .copied()
            .expect("the bound set survives");
        let AuxiliaryAbility::Innate(reloaded_index) = slot else {
            panic!("the slot must still be an innate binding, got {slot:?}")
        };
        assert_eq!(
            reloaded_pool.abilities[reloaded_index], bound_key,
            "the hotbar slot re-pointed off the blade's capstone across a relog"
        );
    }

    /// A spell learned BETWEEN saves must not move an already-bound slot off
    /// its ability.
    ///
    /// This models what a live server does: learn, rebuild the pool, then
    /// re-point the in-memory bindings by key before the next save. That last
    /// step is not optional — see the companion test below for what happens
    /// without it — and every site that rebuilds a pool for a character already
    /// in the world performs it.
    #[test]
    fn learning_a_spell_between_saves_leaves_bound_slots_on_their_ability() {
        let db = TestDb::new();
        let id = create(&db, "uuid-between", &[SPELL_BOOK]);

        // Save 1: bind a slot to a learned spell.
        let mut loaded = load(&db, "uuid-between", id);
        let pool = pool_of(&loaded);
        let index = pool.abilities.len() - 1;
        let bound_key = pool.abilities[index].clone();
        let mut sets = hashbrown::HashMap::new();
        sets.insert((None, None), vec![AuxiliaryAbility::Innate(index)]);
        loaded.active_abilities = ActiveAbilities::from_auxiliary(sets, None);
        save(&db, id, &loaded, &pool);

        // Learn another book, then save 2.
        let mut loaded = load(&db, "uuid-between", id);
        loaded
            .inventory
            .push_spell_group(spell_group(SECOND_SPELL_BOOK))
            .expect("a second, distinct group");
        let grown = pool_of(&loaded);
        // The second book contributes a key that sorts BEFORE the one already
        // bound, so the bound entry genuinely moves within the pool. That is
        // precisely the case a positional binding would get wrong.
        assert!(grown.abilities.len() > pool.abilities.len());
        assert_ne!(
            grown.abilities.iter().position(|k| *k == bound_key),
            pool.abilities.iter().position(|k| *k == bound_key),
            "this test is only meaningful if the bound key actually moves"
        );
        comp::ability::remap_innate_bindings(&mut loaded.active_abilities, &pool, &grown);
        save(&db, id, &loaded, &grown);

        // The slot must still name the ability it was bound to.
        let reloaded = load(&db, "uuid-between", id);
        let reloaded_pool = pool_of(&reloaded);
        let slot = reloaded
            .active_abilities
            .auxiliary_sets
            .get(&(None, None))
            .and_then(|set| set.first())
            .copied()
            .expect("the bound set survives");
        let AuxiliaryAbility::Innate(reloaded_index) = slot else {
            panic!("the slot must still be an innate binding, got {slot:?}")
        };
        assert_eq!(
            reloaded_pool.abilities[reloaded_index], bound_key,
            "learning a spell between saves moved a previously-bound slot"
        );
        // Both books are known now.
        for specifier in [SPELL_BOOK, SECOND_SPELL_BOOK] {
            let book = spell_group(specifier);
            let ItemKind::SpellGroup { spells } = &*book.kind() else {
                unreachable!()
            };
            for key in spells {
                assert!(reloaded.inventory.spell_is_known(key), "{key} was lost");
            }
        }
    }

    /// The companion to the test above, pinning down WHY the remap is
    /// mandatory: skipping it writes the slot out under whatever key happens to
    /// occupy its old index, which is the "my hotbar scrambled" bug.
    #[test]
    fn skipping_the_remap_would_rebind_the_slot_to_the_wrong_ability() {
        let db = TestDb::new();
        let id = create(&db, "uuid-noremap", &[SPELL_BOOK]);

        let mut loaded = load(&db, "uuid-noremap", id);
        let pool = pool_of(&loaded);
        let index = pool.abilities.len() - 1;
        let bound_key = pool.abilities[index].clone();
        let mut sets = hashbrown::HashMap::new();
        sets.insert((None, None), vec![AuxiliaryAbility::Innate(index)]);
        loaded.active_abilities = ActiveAbilities::from_auxiliary(sets, None);
        save(&db, id, &loaded, &pool);

        let mut loaded = load(&db, "uuid-noremap", id);
        loaded
            .inventory
            .push_spell_group(spell_group(SECOND_SPELL_BOOK))
            .expect("a second, distinct group");
        let grown = pool_of(&loaded);
        // Deliberately NO remap here.
        save(&db, id, &loaded, &grown);

        let reloaded = load(&db, "uuid-noremap", id);
        let reloaded_pool = pool_of(&reloaded);
        let AuxiliaryAbility::Innate(reloaded_index) = reloaded
            .active_abilities
            .auxiliary_sets
            .get(&(None, None))
            .and_then(|set| set.first())
            .copied()
            .expect("the set survives")
        else {
            panic!("still an innate binding")
        };
        assert_ne!(
            reloaded_pool.abilities[reloaded_index], bound_key,
            "if this ever passes, index stability was achieved some other way and the remap \
             requirement documented above can be revisited"
        );
    }

    /// 🔴 The trigger-slot twin of
    /// `learning_a_spell_between_saves_leaves_bound_slots_on_their_ability`,
    /// and the one that actually matters: a hotbar slot that silently
    /// re-pointed costs a mis-click, a trigger slot that silently re-points
    /// mints a cooldown-bypass token for the wrong spell and charges up to
    /// thirty-six real-world hours for it.
    ///
    /// Both halves of the fix are exercised at once — the persisted form names
    /// a pool key rather than an index, and the in-memory binding is re-pointed
    /// by `TriggerSlots::remap_innate_bindings` before the next save, exactly
    /// as every live pool-rebuild site does.
    #[test]
    fn learning_a_spell_between_saves_leaves_a_trigger_on_its_ability() {
        use common::comp::{TriggerCondition, TriggerSlot};

        let db = TestDb::new();
        let id = create(&db, "uuid-trigger", &[SPELL_BOOK]);

        // Save 1: point a trigger at a learned spell.
        let mut loaded = load(&db, "uuid-trigger", id);
        let pool = pool_of(&loaded);
        let index = pool.abilities.len() - 1;
        let bound_key = pool.abilities[index].clone();
        assert!(
            loaded.inventory.spell_is_known(&bound_key),
            "the last entry should be a learned spell"
        );
        loaded.trigger_slots.slots[0] = Some(TriggerSlot::from_pool_index(
            index,
            TriggerCondition::HealthBelow(0.25),
        ));
        save(&db, id, &loaded, &pool);

        // Learn a second book whose keys sort before the bound one, then save.
        let mut loaded = load(&db, "uuid-trigger", id);
        loaded
            .inventory
            .push_spell_group(spell_group(SECOND_SPELL_BOOK))
            .expect("a second, distinct group");
        let grown = pool_of(&loaded);
        assert_ne!(
            grown.abilities.iter().position(|k| *k == bound_key),
            pool.abilities.iter().position(|k| *k == bound_key),
            "this test is only meaningful if the bound key actually moves"
        );
        loaded.trigger_slots.remap_innate_bindings(&pool, &grown);
        // The in-memory binding already follows its ability, before any save.
        let AuxiliaryAbility::Innate(live_index) = loaded
            .trigger_slots
            .configured_ability(0)
            .expect("the slot survives the remap")
        else {
            panic!("a trigger slot always resolves to an innate binding")
        };
        assert_eq!(grown.abilities[live_index], bound_key);
        save(&db, id, &loaded, &grown);

        // And so does the persisted one.
        let reloaded = load(&db, "uuid-trigger", id);
        let reloaded_pool = pool_of(&reloaded);
        let AuxiliaryAbility::Innate(reloaded_index) = reloaded
            .trigger_slots
            .configured_ability(0)
            .expect("the trigger survives the reload")
        else {
            panic!("a trigger slot always resolves to an innate binding")
        };
        assert_eq!(
            reloaded_pool.abilities[reloaded_index], bound_key,
            "learning a spell between saves re-pointed a trigger at a different ability"
        );
        assert_eq!(
            reloaded.trigger_slots.get(0).map(|s| s.condition),
            Some(TriggerCondition::HealthBelow(0.25)),
        );
    }

    /// The migration's own contract: a character created BEFORE the spell book
    /// existed gets one, its normal inventory is untouched, and the entity-id
    /// sequence is left consistent so the next allocation cannot collide (the
    /// exact failure the recipe-book migration had to be repaired for).
    #[test]
    fn the_backfill_adds_a_container_without_disturbing_anything_else() {
        let db = TestDb::new();
        // A character with real inventory contents to protect.
        let mut inventory = Inventory::with_empty();
        inventory
            .push(common::comp::Item::new_from_asset_expect(
                "common.items.weapons.sword.starter",
            ))
            .expect("room in a fresh inventory");
        let mut conn = db.connection();
        let mut transaction = conn.connection.transaction().expect("transaction");
        let (id, _) = create_character(
            "uuid-backfill",
            "Testificate",
            components(inventory),
            &mut transaction,
        )
        .expect("character creation");
        transaction.commit().expect("commit");

        let before = load(&db, "uuid-backfill", id);
        let inventory_before: Vec<_> = before
            .inventory
            .slots()
            .flatten()
            .map(|item| item.item_definition_id().to_owned())
            .collect();
        assert!(!inventory_before.is_empty());

        // Simulate the pre-migration state: a character with no spell book at
        // all, exactly as every character looked before this schema change.
        {
            let conn = db.connection();
            conn.execute(
                "DELETE FROM item WHERE parent_container_item_id IN (
                     SELECT item_id FROM item
                     WHERE position = 'spell_book' AND parent_container_item_id = ?1
                 )",
                [id.0],
            )
            .expect("empty the spell book");
            conn.execute(
                "DELETE FROM item WHERE position = 'spell_book' AND parent_container_item_id = ?1",
                [id.0],
            )
            .expect("remove the spell book container");
        }
        run_the_spell_book_backfill(&db);

        let after = load(&db, "uuid-backfill", id);
        let inventory_after: Vec<_> = after
            .inventory
            .slots()
            .flatten()
            .map(|item| item.item_definition_id().to_owned())
            .collect();
        assert_eq!(
            inventory_before, inventory_after,
            "the backfill disturbed the character's normal inventory"
        );
        // The backfilled book is empty (the migration never invents contents),
        // which is exactly what a pre-existing character should have.
        assert_eq!(after.inventory.spell_book_len(), 0);

        // Every item row must have an entity row, and the AUTOINCREMENT
        // sequence must be at or above the highest id in use — the invariant
        // whose violation forced a follow-up fix on the recipe book.
        let conn = db.connection();
        let orphans: i64 = conn
            .query_row(
                "SELECT COUNT(1) FROM item WHERE item_id NOT IN (SELECT entity_id FROM entity)",
                [],
                |row| row.get(0),
            )
            .expect("orphan count");
        assert_eq!(orphans, 0, "the backfill left item rows with no entity row");

        let (seq, max_item): (i64, i64) = conn
            .query_row(
                "SELECT (SELECT seq FROM sqlite_sequence WHERE name = 'entity'),
                        (SELECT MAX(item_id) FROM item)",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .expect("sequence state");
        assert!(
            seq >= max_item,
            "the entity sequence ({seq}) fell behind the highest item id ({max_item}); the next \
             allocation would collide"
        );

        // Running the backfill again must be a no-op, never a duplicate.
        run_the_spell_book_backfill(&db);
        let books: i64 = conn
            .query_row(
                "SELECT COUNT(1) FROM item WHERE position = 'spell_book'",
                [],
                |row| row.get(0),
            )
            .expect("book count");
        assert_eq!(books, 1);
    }

    /// Executes the shipped backfill SQL directly.
    ///
    /// `run_migrations` cannot be reused here: refinery records the migration
    /// as applied on the database's first start, so re-running it is a
    /// no-op. The point of these assertions is the SQL itself, so it is
    /// executed verbatim from the same file the runner embeds.
    fn run_the_spell_book_backfill(db: &TestDb) {
        let conn = db.connection();
        conn.execute_batch(include_str!("../../migrations/V78__spell_book.sql"))
            .expect("the backfill SQL runs");
    }

    /// End-to-end: a Blade boon's `blade_exp`/`blade_name` survive a real
    /// save/load round trip through SQLite, and switching the boon away and
    /// back (never touching those two columns in between, exactly like
    /// `/pact boon`) resumes from the same `blade_exp` rather than resetting
    /// it -- the persistence half of the "survives a boon change" contract
    /// (the in-memory half is covered by `conversions.rs`'s own test).
    #[test]
    fn pact_blade_exp_and_name_survive_a_save_load_round_trip_and_a_boon_switch() {
        use common::comp::{Ethos, Pact, PactBoon, PactStanding, ethos::Moral};

        let db = TestDb::new();
        let id = create(&db, "uuid-blade-xp", &[]);

        let mut loaded = load(&db, "uuid-blade-xp", id);
        let pool = pool_of(&loaded);
        loaded.pact = Pact {
            standing: PactStanding::Bound,
            boon: Some(PactBoon::Blade),
            blade_summoned: true,
            blade_exp: 26_500,
            blade_name: None,
            ..Pact::default()
        };
        loaded.ethos = Ethos::from_box(common::comp::ethos::Order::Neutral, Moral::Good);
        save(&db, id, &loaded, &pool);

        let reloaded = load(&db, "uuid-blade-xp", id);
        assert_eq!(reloaded.pact.blade_exp, 26_500);
        assert_eq!(reloaded.pact.blade_tier(), 2, "25,000 XP is tier 2");

        // Switch the boon away (mirrors `/pact boon chain`, which zeroes
        // `blade_summoned` but never touches `blade_exp`/`blade_name`) and
        // save again -- the columns must not reset.
        let mut switched = reloaded;
        switched.pact = Pact {
            boon: Some(PactBoon::Chain),
            blade_summoned: false,
            ..switched.pact
        };
        save(&db, id, &switched, &pool);
        let after_switch = load(&db, "uuid-blade-xp", id);
        assert_eq!(after_switch.pact.boon, Some(PactBoon::Chain));
        assert_eq!(
            after_switch.pact.blade_exp, 26_500,
            "blade_exp must survive a boon change"
        );

        // Switch back to Blade -- resumes from the same total, not 0.
        let mut resumed = after_switch;
        resumed.pact = Pact {
            boon: Some(PactBoon::Blade),
            blade_summoned: true,
            ..resumed.pact
        };
        save(&db, id, &resumed, &pool);
        let final_load = load(&db, "uuid-blade-xp", id);
        assert_eq!(
            final_load.pact.blade_exp, 26_500,
            "re-taking Blade must resume, not reset, blade_exp"
        );
    }
}

/// NH-79: end-to-end tests for the character-summary/rename path added in
/// this phase -- name validation, the new `character.alias` uniqueness
/// constraint (migration V86), the rename ownership check, and
/// `load_character_summaries`/`character_level`. Same "run the real
/// migrations" reasoning as `spell_book_persistence_tests` above: the point
/// is exercising the interaction with the schema (the actual `UNIQUE INDEX
/// ... COLLATE NOCASE`), not just the Rust-side validator alone (that's
/// already covered by `common::character`'s own unit tests).
#[cfg(test)]
mod nh79_character_summary_tests {
    use super::*;
    use crate::persistence::{ConnectionMode, DatabaseSettings, SqlLogMode, establish_connection};
    use common::comp::{ActiveAbilities, CharacterClass};

    struct TestDb {
        _dir: tempfile::TempDir,
        settings: DatabaseSettings,
    }

    impl TestDb {
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

        fn connection(&self) -> crate::persistence::VelorenConnection {
            establish_connection(&self.settings, ConnectionMode::ReadWrite)
        }
    }

    fn components() -> PersistedComponents {
        let body = comp::Body::Humanoid(comp::humanoid::Body::random());
        PersistedComponents {
            body,
            hardcore: None,
            character_class: CharacterClass::single(comp::ClassKind::Warrior),
            stats: comp::Stats::new(Content::Plain("Testificate".to_owned()), body),
            skill_set: comp::SkillSet::default(),
            inventory: Inventory::with_empty(),
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

    /// Creates a character and commits on success; on failure the
    /// transaction is dropped uncommitted, which rolls it back (rusqlite's
    /// default `DropBehavior` for an unfinished `Transaction`).
    fn create(db: &TestDb, uuid: &str, alias: &str) -> Result<CharacterId, PersistenceError> {
        let mut conn = db.connection();
        let mut transaction = conn.connection.transaction().expect("transaction");
        let result =
            create_character(uuid, alias, components(), &mut transaction).map(|(id, _)| id);
        if result.is_ok() {
            transaction.commit().expect("commit");
        }
        result
    }

    #[test]
    fn create_character_rejects_invalid_name() {
        let db = TestDb::new();
        let err = create(&db, "uuid-invalid-name", "   ").unwrap_err();
        assert!(matches!(err, PersistenceError::CharacterNameInvalid(_)));
    }

    #[test]
    fn create_character_rejects_duplicate_name_case_insensitive() {
        let db = TestDb::new();
        create(&db, "uuid-a", "Aragorn").expect("first creation succeeds");
        let err = create(&db, "uuid-b", "aragorn").unwrap_err();
        assert!(matches!(err, PersistenceError::CharacterNameUnavailable));
    }

    /// The `character.alias` uniqueness index folds case (`COLLATE NOCASE`)
    /// but not whitespace, so `create_character`/`rename_character` must
    /// normalize whitespace themselves before the uniqueness check --
    /// otherwise `" Aragorn"` and `"Aragorn"` would both be accepted as
    /// distinct rows, letting a player create a visually-identical name.
    #[test]
    fn create_character_rejects_duplicate_name_differing_only_in_whitespace() {
        let db = TestDb::new();
        create(&db, "uuid-a", "Aragorn").expect("first creation succeeds");
        let err = create(&db, "uuid-b", "  Aragorn ").unwrap_err();
        assert!(matches!(err, PersistenceError::CharacterNameUnavailable));
    }

    #[test]
    fn create_character_stores_the_normalized_name() {
        let db = TestDb::new();
        let id = create(&db, "uuid-normalize", "  Two   Words  ").expect("creation");

        let conn = db.connection();
        let summaries = load_character_summaries("uuid-normalize", &conn).expect("load");
        assert_eq!(
            summaries
                .iter()
                .find(|s| s.character_id == id)
                .unwrap()
                .alias,
            "Two Words"
        );
    }

    #[test]
    fn rename_character_happy_path() {
        let db = TestDb::new();
        let id = create(&db, "uuid-rename", "Original").expect("creation");

        let mut conn = db.connection();
        let mut transaction = conn.connection.transaction().expect("transaction");
        rename_character("uuid-rename", id, "Renamed", &mut transaction).expect("rename");
        transaction.commit().expect("commit");

        let conn = db.connection();
        let summaries = load_character_summaries("uuid-rename", &conn).expect("load");
        assert_eq!(summaries.len(), 1);
        assert_eq!(summaries[0].alias, "Renamed");
    }

    #[test]
    fn rename_character_rejects_when_not_owner() {
        let db = TestDb::new();
        let id = create(&db, "uuid-owner", "Owned").expect("creation");

        let mut conn = db.connection();
        let mut transaction = conn.connection.transaction().expect("transaction");
        let err = rename_character("uuid-not-owner", id, "Stolen", &mut transaction).unwrap_err();
        assert!(matches!(err, PersistenceError::CharacterNotFound));
    }

    #[test]
    fn rename_character_rejects_invalid_name() {
        let db = TestDb::new();
        let id = create(&db, "uuid-bad-rename", "Fine").expect("creation");

        let mut conn = db.connection();
        let mut transaction = conn.connection.transaction().expect("transaction");
        let err = rename_character("uuid-bad-rename", id, "", &mut transaction).unwrap_err();
        assert!(matches!(err, PersistenceError::CharacterNameInvalid(_)));
    }

    #[test]
    fn rename_character_rejects_taken_name_case_insensitive() {
        let db = TestDb::new();
        create(&db, "uuid-taken-a", "Foo").expect("first creation");
        let bar_id = create(&db, "uuid-taken-b", "Bar").expect("second creation");

        let mut conn = db.connection();
        let mut transaction = conn.connection.transaction().expect("transaction");
        let err = rename_character("uuid-taken-b", bar_id, "foo", &mut transaction).unwrap_err();
        assert!(matches!(err, PersistenceError::CharacterNameUnavailable));
    }

    #[test]
    fn load_character_summaries_filters_by_uuid_and_computes_level() {
        let db = TestDb::new();
        create(&db, "uuid-list-a", "First").expect("creation a");
        create(&db, "uuid-list-b", "Second").expect("creation b");

        let conn = db.connection();
        let summaries = load_character_summaries("uuid-list-a", &conn).expect("load");
        assert_eq!(summaries.len(), 1);
        assert_eq!(summaries[0].alias, "First");
        assert_eq!(summaries[0].level, 1, "a fresh character starts at level 1");
        assert_eq!(summaries[0].waypoint, None);
    }

    #[test]
    fn load_character_summaries_is_empty_for_unknown_uuid() {
        let db = TestDb::new();
        let conn = db.connection();
        let summaries = load_character_summaries("uuid-does-not-exist", &conn).expect("load");
        assert!(summaries.is_empty());
    }
}

/// End-to-end persistence tests for per-character suspension: suspend, check
/// it's visible everywhere it needs to be, unsuspend, check it's gone.
#[cfg(test)]
mod character_suspension_persistence_tests {
    use super::*;
    use crate::persistence::{ConnectionMode, DatabaseSettings, SqlLogMode, establish_connection};
    use chrono::SubsecRound;
    use common::comp::{ActiveAbilities, CharacterClass};

    struct TestDb {
        _dir: tempfile::TempDir,
        settings: DatabaseSettings,
    }

    impl TestDb {
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

        fn connection(&self) -> crate::persistence::VelorenConnection {
            establish_connection(&self.settings, ConnectionMode::ReadWrite)
        }
    }

    fn components() -> PersistedComponents {
        let body = comp::Body::Humanoid(comp::humanoid::Body::random());
        PersistedComponents {
            body,
            hardcore: None,
            character_class: CharacterClass::single(comp::ClassKind::Warrior),
            stats: comp::Stats::new(Content::Plain("Testificate".to_owned()), body),
            skill_set: comp::SkillSet::default(),
            inventory: Inventory::with_empty(),
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

    fn create(db: &TestDb, uuid: &str, alias: &str) -> CharacterId {
        let mut conn = db.connection();
        let mut transaction = conn.connection.transaction().expect("transaction");
        let (id, _) =
            create_character(uuid, alias, components(), &mut transaction).expect("creation");
        transaction.commit().expect("commit");
        id
    }

    /// Truncated to whole seconds: `to_rfc3339`/`parse_from_rfc3339` round
    /// trips exactly at that resolution, but a raw `Utc::now()` carries
    /// sub-second precision that would make an `assert_eq!` against the
    /// reloaded value flaky.
    fn now() -> DateTime<Utc> { Utc::now().trunc_subsecs(0) }

    #[test]
    fn a_suspended_character_is_visible_in_the_character_list_with_its_reason() {
        let db = TestDb::new();
        let id = create(&db, "uuid-suspend-list", "Suspecto");

        let mut conn = db.connection();
        let mut transaction = conn.connection.transaction().expect("transaction");
        suspend_character(
            id,
            "duped items via a bugged trade",
            "operator-uuid",
            now(),
            None,
            &mut transaction,
        )
        .expect("suspend");
        transaction.commit().expect("commit");

        let conn = db.connection();
        let list = load_character_list("uuid-suspend-list", &conn).expect("load list");
        let entry = list.iter().find(|c| c.character.id == Some(id)).unwrap();
        let suspension = entry
            .suspended
            .as_ref()
            .expect("the character list must surface the real suspension");
        assert_eq!(suspension.reason, "duped items via a bugged trade");
        assert_eq!(
            suspension.end_date, None,
            "a permanent suspension has no end date"
        );
    }

    #[test]
    fn suspend_check_unsuspend_check_round_trip() {
        let db = TestDb::new();
        let id = create(&db, "uuid-suspend-roundtrip", "Roundtrip");
        let suspended_at = now();
        let end_date = suspended_at + chrono::Duration::seconds(3600);

        // Not suspended yet.
        let conn = db.connection();
        assert!(
            get_character_suspension(id, &conn)
                .expect("query")
                .is_none()
        );
        drop(conn);

        // Suspend it.
        let mut conn = db.connection();
        let mut transaction = conn.connection.transaction().expect("transaction");
        suspend_character(
            id,
            "bugged into invincibility",
            "operator-uuid",
            suspended_at,
            Some(end_date),
            &mut transaction,
        )
        .expect("suspend");
        transaction.commit().expect("commit");

        // Enforced: visible via the direct lookup and via the startup-load path.
        let conn = db.connection();
        let record = get_character_suspension(id, &conn)
            .expect("query")
            .expect("must be suspended now");
        assert_eq!(record.reason, "bugged into invincibility");
        assert_eq!(record.suspended_by_operator_uuid, "operator-uuid");
        assert_eq!(record.suspended_at, suspended_at);
        assert_eq!(record.end_date, Some(end_date));

        let all = load_all_character_suspensions(&conn).expect("load all");
        assert_eq!(all.get(&id).map(|r| r.reason.clone()), Some(record.reason));
        drop(conn);

        // Unsuspend it.
        let mut conn = db.connection();
        let mut transaction = conn.connection.transaction().expect("transaction");
        unsuspend_character(id, &mut transaction).expect("unsuspend");
        transaction.commit().expect("commit");

        // Cleared: neither lookup nor the character list shows it anymore.
        let conn = db.connection();
        assert!(
            get_character_suspension(id, &conn)
                .expect("query")
                .is_none()
        );
        let all = load_all_character_suspensions(&conn).expect("load all");
        assert!(!all.contains_key(&id));
        let list = load_character_list("uuid-suspend-roundtrip", &conn).expect("load list");
        let entry = list.iter().find(|c| c.character.id == Some(id)).unwrap();
        assert!(entry.suspended.is_none());
    }

    #[test]
    fn a_re_suspend_replaces_rather_than_stacks() {
        let db = TestDb::new();
        let id = create(&db, "uuid-suspend-replace", "Repeato");

        let mut conn = db.connection();
        let mut transaction = conn.connection.transaction().expect("transaction");
        suspend_character(id, "first reason", "op-1", now(), None, &mut transaction)
            .expect("first suspend");
        transaction.commit().expect("commit");

        let mut conn = db.connection();
        let mut transaction = conn.connection.transaction().expect("transaction");
        suspend_character(id, "second reason", "op-2", now(), None, &mut transaction)
            .expect("second suspend");
        transaction.commit().expect("commit");

        let conn = db.connection();
        let record = get_character_suspension(id, &conn)
            .expect("query")
            .expect("still suspended");
        assert_eq!(record.reason, "second reason");
        assert_eq!(record.suspended_by_operator_uuid, "op-2");
        assert_eq!(
            load_all_character_suspensions(&conn)
                .expect("load all")
                .len(),
            1,
            "a re-suspend must replace the row, not add a second one"
        );
    }

    #[test]
    fn unsuspend_an_already_clear_character_is_not_an_error() {
        let db = TestDb::new();
        let id = create(&db, "uuid-suspend-idempotent", "Clearskies");

        let mut conn = db.connection();
        let mut transaction = conn.connection.transaction().expect("transaction");
        unsuspend_character(id, &mut transaction).expect("unsuspend never-suspended character");
        transaction.commit().expect("commit");
    }

    #[test]
    fn character_owner_uuid_resolves_the_owning_account() {
        let db = TestDb::new();
        let id = create(&db, "uuid-suspend-owner", "Owned");

        let conn = db.connection();
        assert_eq!(
            character_owner_uuid(id, &conn).expect("query"),
            Some("uuid-suspend-owner".to_owned())
        );
        assert_eq!(
            character_owner_uuid(CharacterId(id.0 + 1_000_000), &conn).expect("query"),
            None
        );
    }
}
