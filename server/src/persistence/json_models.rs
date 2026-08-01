use common::comp;
use common_base::dev_panic;
use hashbrown::HashMap;
use serde::{Deserialize, Serialize};
use std::{num::NonZeroU32, string::ToString};
use vek::{Vec2, Vec3};

#[derive(Serialize, Deserialize)]
pub struct HumanoidBody {
    pub species: u8,
    pub body_type: u8,
    pub hair_style: u8,
    pub beard: u8,
    pub eyes: u8,
    pub accessory: u8,
    pub hair_color: u8,
    pub skin: u8,
    pub eye_color: u8,
    #[serde(default = "default_height_scale")]
    pub height_scale: u8,
}

fn default_height_scale() -> u8 { u8::MAX / 2 }

impl From<&comp::humanoid::Body> for HumanoidBody {
    fn from(body: &comp::humanoid::Body) -> Self {
        HumanoidBody {
            species: body.species as u8,
            body_type: body.body_type as u8,
            hair_style: body.hair_style,
            beard: body.beard,
            eyes: body.eyes,
            accessory: body.accessory,
            hair_color: body.hair_color,
            skin: body.skin,
            eye_color: body.eye_color,
            height_scale: body.height_scale,
        }
    }
}

/// A serializable model used to represent a generic Body. Since all variants
/// of Body except Humanoid (currently) have the same struct layout, a single
/// struct is used for persistence conversions.
#[derive(Serialize, Deserialize)]
pub struct GenericBody {
    pub species: String,
    pub body_type: String,
}

macro_rules! generic_body_from_impl {
    ($body_type:ty) => {
        impl From<&$body_type> for GenericBody {
            fn from(body: &$body_type) -> Self {
                GenericBody {
                    species: body.species.to_string(),
                    body_type: body.body_type.to_string(),
                }
            }
        }
    };
}

generic_body_from_impl!(comp::quadruped_low::Body);
generic_body_from_impl!(comp::quadruped_medium::Body);
generic_body_from_impl!(comp::quadruped_small::Body);
generic_body_from_impl!(comp::bird_medium::Body);
generic_body_from_impl!(comp::crustacean::Body);

#[derive(Serialize, Deserialize)]
pub struct CharacterPosition {
    pub waypoint: Option<Vec3<f32>>,
    pub map_marker: Option<Vec2<i32>>,
}

pub fn skill_group_to_db_string(skill_group: comp::skillset::SkillGroupKind) -> String {
    use comp::{class::ClassKind, item::tool::ToolKind, skillset::SkillGroupKind::*};
    let skill_group_string = match skill_group {
        General => "General",
        Weapon(ToolKind::Sword) => "Weapon Sword",
        Weapon(ToolKind::Axe) => "Weapon Axe",
        Weapon(ToolKind::Hammer) => "Weapon Hammer",
        Weapon(ToolKind::Bow) => "Weapon Bow",
        Weapon(ToolKind::Staff) => "Weapon Staff",
        Weapon(ToolKind::Sceptre) => "Weapon Sceptre",
        Weapon(ToolKind::Pick) => "Weapon Pick",
        Class(ClassKind::Warrior) => "Class Warrior",
        Class(ClassKind::Mage) => "Class Mage",
        Class(ClassKind::Cleric) => "Class Cleric",
        Class(ClassKind::Rogue) => "Class Rogue",
        Class(ClassKind::Barbarian) => "Class Barbarian",
        Class(ClassKind::Sorcerer) => "Class Sorcerer",
        Class(ClassKind::Warlock) => "Class Warlock",
        Class(ClassKind::Bard) => "Class Bard",
        Class(ClassKind::Paladin) => "Class Paladin",
        Class(ClassKind::Druid) => "Class Druid",
        Class(ClassKind::Ranger) => "Class Ranger",
        Class(ClassKind::Monk) => "Class Monk",
        Class(ClassKind::Artificer) => "Class Artificer",
        Class(ClassKind::BloodSlayer) => "Class BloodSlayer",
        Feats => "Feats",
        // Adventurer has no class tree; a Class(Adventurer) group reaching
        // persistence is a bug, consistent with the unsupported-weapon arm.
        Class(ClassKind::Adventurer) => panic!(
            "Tried to add unsupported skill group to database: {:?}",
            skill_group
        ),
        Weapon(ToolKind::Dagger)
        | Weapon(ToolKind::Shield)
        | Weapon(ToolKind::Spear)
        | Weapon(ToolKind::Blowgun)
        | Weapon(ToolKind::Debug)
        | Weapon(ToolKind::Farming)
        | Weapon(ToolKind::Instrument)
        | Weapon(ToolKind::Throwable)
        | Weapon(ToolKind::Empty)
        | Weapon(ToolKind::Natural)
        | Weapon(ToolKind::Shovel)
        | Weapon(ToolKind::Tome)
        | Weapon(ToolKind::HolySymbol)
        | Weapon(ToolKind::Focus) => panic!(
            "Tried to add unsupported skill group to database: {:?}",
            skill_group
        ),
    };
    skill_group_string.to_string()
}

pub fn db_string_to_skill_group(skill_group_string: &str) -> comp::skillset::SkillGroupKind {
    use comp::{class::ClassKind, item::tool::ToolKind, skillset::SkillGroupKind::*};
    match skill_group_string {
        "General" => General,
        "Weapon Sword" => Weapon(ToolKind::Sword),
        "Weapon Axe" => Weapon(ToolKind::Axe),
        "Weapon Hammer" => Weapon(ToolKind::Hammer),
        "Weapon Bow" => Weapon(ToolKind::Bow),
        "Weapon Staff" => Weapon(ToolKind::Staff),
        "Weapon Sceptre" => Weapon(ToolKind::Sceptre),
        "Weapon Pick" => Weapon(ToolKind::Pick),
        "Class Warrior" => Class(ClassKind::Warrior),
        "Class Mage" => Class(ClassKind::Mage),
        "Class Cleric" => Class(ClassKind::Cleric),
        "Class Rogue" => Class(ClassKind::Rogue),
        "Class Barbarian" => Class(ClassKind::Barbarian),
        "Class Sorcerer" => Class(ClassKind::Sorcerer),
        "Class Warlock" => Class(ClassKind::Warlock),
        "Class Bard" => Class(ClassKind::Bard),
        "Class Paladin" => Class(ClassKind::Paladin),
        "Class Druid" => Class(ClassKind::Druid),
        "Class Ranger" => Class(ClassKind::Ranger),
        "Class Monk" => Class(ClassKind::Monk),
        "Class Artificer" => Class(ClassKind::Artificer),
        "Class BloodSlayer" => Class(ClassKind::BloodSlayer),
        "Feats" => Feats,

        _ => panic!(
            "Tried to convert an unsupported string from the database: {}",
            skill_group_string
        ),
    }
}

pub fn class_to_db_string(class: comp::class::ClassKind) -> String {
    use comp::class::ClassKind::*;
    match class {
        Adventurer => "Adventurer",
        Warrior => "Warrior",
        Mage => "Mage",
        Cleric => "Cleric",
        Rogue => "Rogue",
        Barbarian => "Barbarian",
        Sorcerer => "Sorcerer",
        Warlock => "Warlock",
        Bard => "Bard",
        Paladin => "Paladin",
        Druid => "Druid",
        Ranger => "Ranger",
        Monk => "Monk",
        Artificer => "Artificer",
        BloodSlayer => "BloodSlayer",
    }
    .to_string()
}

/// Unlike the skill-group converter this never panics: unknown strings fall
/// back to Adventurer with a warning so a DB downgrade never bricks a save.
pub fn db_string_to_class(class_string: &str) -> comp::class::ClassKind {
    comp::class::ClassKind::ALL
        .into_iter()
        .find(|class| class_to_db_string(*class) == class_string)
        .unwrap_or_else(|| {
            tracing::warn!(unknown = ?class_string, "Unknown class in database, defaulting to Adventurer");
            comp::class::ClassKind::Adventurer
        })
}

/// BL-31: db-string for `BackgroundKind` variants.
pub fn background_to_db_string(background: comp::background::BackgroundKind) -> String {
    background.keyword().to_string()
}

/// Unlike the skill-group converter this never panics: unknown or
/// unrecognized strings fall back to `None` (P0 §Q1's "Uncommitted") with a
/// warning so a DB downgrade or a future-version string (including the
/// removed `"custom"` value) never bricks a save.
pub fn db_string_to_background(
    background_string: &str,
) -> Option<comp::background::BackgroundKind> {
    comp::background::BackgroundKind::from_keyword(background_string).or_else(|| {
        tracing::warn!(
            unknown = ?background_string,
            "Unknown background in database, defaulting to Uncommitted (None)"
        );
        None
    })
}

/// On-disk form of one reactive trigger slot.
///
/// Deliberately its own type rather than `serde`-ing the component: the live
/// component carries a transient firing state and an in-game `Time` projection
/// that are meaningless across a restart, while the column must carry the
/// authoritative wall-clock instant that the wire format deliberately drops.
#[derive(Serialize, Deserialize)]
pub struct DatabaseTriggerSlot {
    slot: u8,
    ability: comp::ability::AuxiliaryAbility,
    condition: comp::TriggerCondition,
    /// RFC-3339 UTC; absent when the slot is not cooling down.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    ready_at: Option<chrono::DateTime<chrono::Utc>>,
}

/// Serialise a character's trigger slots for the `character.trigger_slots`
/// column. `None` (a SQL NULL) when nothing is configured, so a character that
/// never used the feature costs nothing.
///
/// The transient firing state is never written: a slot that was mid-cast when
/// the server saved re-derives as ready, which is the safe direction (it
/// re-fires when its condition next holds, rather than holding an
/// authorisation token no cast will ever claim).
pub fn trigger_slots_to_db_string(slots: &comp::TriggerSlots) -> Option<String> {
    let rows: Vec<DatabaseTriggerSlot> = slots
        .slots
        .iter()
        .enumerate()
        .filter_map(|(index, slot)| {
            let slot = slot.as_ref()?;
            Some(DatabaseTriggerSlot {
                slot: index as u8,
                ability: slot.ability,
                condition: slot.condition,
                ready_at: slot.state.ready_at(),
            })
        })
        .collect();

    if rows.is_empty() {
        return None;
    }
    serde_json::to_string(&rows)
        .inspect_err(|err| {
            tracing::error!(?err, "Failed to serialize trigger slots; dropping them");
        })
        .ok()
}

/// Inverse of [`trigger_slots_to_db_string`].
///
/// 🔴 A restored cooling slot's in-game projection is left infinite on
/// purpose: only `TriggerSlots::reproject_cooldowns` — which reads the real
/// clock once, at character load — may make it finite. A caller that forgets
/// leaves the slot cooling forever instead of instantly ready, so the failure
/// mode points away from the exploit.
///
/// Malformed JSON yields no slots rather than failing the load: a character
/// must never be locked out by a bad trigger payload.
pub fn db_string_to_trigger_slots(payload: Option<&str>) -> comp::TriggerSlots {
    use common::{comp::trigger::SlotState, resources::Time};

    let mut slots = comp::TriggerSlots::default();
    let Some(payload) = payload else {
        return slots;
    };
    let rows: Vec<DatabaseTriggerSlot> = match serde_json::from_str(payload) {
        Ok(rows) => rows,
        Err(err) => {
            tracing::warn!(?err, "Unreadable trigger slots in database, ignoring them");
            return slots;
        },
    };

    for row in rows {
        let Some(dest) = slots.slots.get_mut(usize::from(row.slot)) else {
            tracing::warn!(slot = row.slot, "Trigger slot index out of range, ignoring");
            continue;
        };
        let state = match row.ready_at {
            Some(ready_at) => SlotState::CoolingDown {
                ready_at: Some(ready_at),
                ready_at_time: Time(f64::INFINITY),
            },
            None => SlotState::Ready,
        };
        *dest = Some(comp::TriggerSlot {
            ability: row.ability,
            condition: row.condition,
            state,
        });
    }
    slots
}

#[derive(Serialize, Deserialize)]
pub struct DatabaseAbilitySet {
    mainhand: String,
    offhand: String,
    abilities: Vec<String>,
}

/// Xindeler: the on-disk form for an `Innate` slot.
///
/// `AuxiliaryAbility::Innate(i)` indexes into `AbilityPool::abilities`, whose
/// contents are content-derived: a class's spell list grows and shrinks as the
/// compendium does, and new entries land in the middle of the ordering rather
/// than at the end. Persisting the raw index would therefore silently re-point
/// every bound slot above an insertion, quietly rearranging a player's action
/// bar after a content patch. Storing the pool *key* instead makes a slot
/// follow its ability, whatever position that ability ends up in.
///
/// The legacy positional form is still accepted on read (see
/// [`aux_ability_from_string`]), so old rows keep working and each character's
/// row is rewritten in the key form the first time it is saved — no migration.
const INNATE_KEY_PREFIX: &str = "Innate:key:";

fn aux_ability_to_string(
    ability: comp::ability::AuxiliaryAbility,
    ability_pool: &comp::ability::AbilityPool,
) -> String {
    use common::comp::ability::AuxiliaryAbility;
    match ability {
        AuxiliaryAbility::MainWeapon(index) => format!("Main Weapon:index:{}", index),
        AuxiliaryAbility::OffWeapon(index) => format!("Off Weapon:index:{}", index),
        AuxiliaryAbility::Glider(index) => format!("Glider:index:{}", index),
        AuxiliaryAbility::Innate(index) => match ability_pool.abilities.get(index) {
            Some(key) => format!("{}{}", INNATE_KEY_PREFIX, key),
            // Should never happen: an `Innate` slot always names a pool entry.
            // Falling back to the positional form loses nothing that the old
            // format did not already lose.
            None => format!("Innate:index:{}", index),
        },
        AuxiliaryAbility::Empty => String::from("Empty"),
    }
}

fn aux_ability_from_string(
    ability: &str,
    ability_pool: &comp::ability::AbilityPool,
) -> comp::ability::AuxiliaryAbility {
    use common::comp::ability::AuxiliaryAbility;
    // Key form first: it does not contain the `:index:` separator the rest of
    // this function splits on.
    if let Some(key) = ability.strip_prefix(INNATE_KEY_PREFIX) {
        return match ability_pool.abilities.iter().position(|k| k == key) {
            Some(index) => AuxiliaryAbility::Innate(index),
            // Deliberately NOT a `dev_panic!`, unlike every other failure path
            // here: an ability that is no longer in the pool means the content
            // changed (a spell was removed) or the character no longer holds
            // the class that granted it. Both are legitimate — the slot just
            // empties.
            None => {
                tracing::debug!(
                    ?key,
                    "Persisted innate ability is no longer in this character's pool; clearing the \
                     slot"
                );
                AuxiliaryAbility::Empty
            },
        };
    }
    let mut parts = ability.split(":index:");
    match parts.next() {
        Some("Main Weapon") => match parts
            .next()
            .map(|index| index.parse::<usize>().map_err(|_| index))
        {
            Some(Ok(index)) => AuxiliaryAbility::MainWeapon(index),
            Some(Err(error)) => {
                dev_panic!(format!(
                    "Conversion from database to ability set failed. Unable to parse index for \
                     mainhand abilities: {}",
                    error
                ));
                AuxiliaryAbility::Empty
            },
            None => {
                dev_panic!(String::from(
                    "Conversion from database to ability set failed. Unable to find an index for \
                     mainhand abilities"
                ));
                AuxiliaryAbility::Empty
            },
        },
        Some("Off Weapon") => match parts
            .next()
            .map(|index| index.parse::<usize>().map_err(|_| index))
        {
            Some(Ok(index)) => AuxiliaryAbility::OffWeapon(index),
            Some(Err(error)) => {
                dev_panic!(format!(
                    "Conversion from database to ability set failed. Unable to parse index for \
                     offhand abilities: {}",
                    error
                ));
                AuxiliaryAbility::Empty
            },
            None => {
                dev_panic!(String::from(
                    "Conversion from database to ability set failed. Unable to find an index for \
                     offhand abilities"
                ));
                AuxiliaryAbility::Empty
            },
        },
        Some("Glider") => match parts
            .next()
            .map(|index| index.parse::<usize>().map_err(|_| index))
        {
            Some(Ok(index)) => AuxiliaryAbility::Glider(index),
            Some(Err(error)) => {
                dev_panic!(format!(
                    "Conversion from database to ability set failed. Unable to parse index for \
                     offhand abilities: {}",
                    error
                ));
                AuxiliaryAbility::Empty
            },
            None => {
                dev_panic!(String::from(
                    "Conversion from database to ability set failed. Unable to find an index for \
                     offhand abilities"
                ));
                AuxiliaryAbility::Empty
            },
        },
        Some("Innate") => match parts
            .next()
            .map(|index| index.parse::<usize>().map_err(|_| index))
        {
            Some(Ok(index)) => AuxiliaryAbility::Innate(index),
            Some(Err(error)) => {
                dev_panic!(format!(
                    "Conversion from database to ability set failed. Unable to parse index for \
                     innate abilities: {}",
                    error
                ));
                AuxiliaryAbility::Empty
            },
            None => {
                dev_panic!(String::from(
                    "Conversion from database to ability set failed. Unable to find an index for \
                     innate abilities"
                ));
                AuxiliaryAbility::Empty
            },
        },
        Some("Empty") => AuxiliaryAbility::Empty,
        unknown => {
            dev_panic!(format!(
                "Conversion from database to ability set failed. Unknown auxiliary ability: {:#?}",
                unknown
            ));
            AuxiliaryAbility::Empty
        },
    }
}

fn tool_kind_to_string(tool: Option<comp::item::tool::ToolKind>) -> String {
    use common::comp::item::tool::ToolKind::*;
    String::from(match tool {
        Some(Sword) => "Sword",
        Some(Axe) => "Axe",
        Some(Hammer) => "Hammer",
        Some(Bow) => "Bow",
        Some(Staff) => "Staff",
        Some(Sceptre) => "Sceptre",
        Some(Tome) => "Tome",
        Some(HolySymbol) => "HolySymbol",
        Some(Focus) => "Focus",
        Some(Dagger) => "Dagger",
        Some(Shield) => "Shield",
        Some(Spear) => "Spear",
        Some(Blowgun) => "Blowgun",
        Some(Pick) => "Pick",
        Some(Shovel) => "Shovel",

        // Toolkinds that are not anticipated to have many active abilities (if any at all)
        Some(Farming) => "Farming",
        Some(Debug) => "Debug",
        Some(Natural) => "Natural",
        Some(Instrument) => "Instrument",
        Some(Throwable) => "Throwable",
        Some(Empty) => "Empty",
        None => "None",
    })
}

fn tool_kind_from_string(tool: String) -> Option<comp::item::tool::ToolKind> {
    use common::comp::item::tool::ToolKind::*;
    match tool.as_str() {
        "Sword" => Some(Sword),
        "Axe" => Some(Axe),
        "Hammer" => Some(Hammer),
        "Bow" => Some(Bow),
        "Staff" => Some(Staff),
        "Sceptre" => Some(Sceptre),
        "Tome" => Some(Tome),
        "HolySymbol" => Some(HolySymbol),
        "Focus" => Some(Focus),
        "Dagger" => Some(Dagger),
        "Shield" => Some(Shield),
        "Spear" => Some(Spear),
        "Blowgun" => Some(Blowgun),
        "Pick" => Some(Pick),
        "Farming" => Some(Farming),
        "Debug" => Some(Debug),
        "Natural" => Some(Natural),
        "Empty" => Some(Empty),
        "None" => None,
        unknown => {
            dev_panic!(format!(
                "Conversion from database to ability set failed. Unknown toolkind: {:#?}",
                unknown
            ));
            None
        },
    }
}

pub fn active_abilities_to_db_model(
    active_abilities: &comp::ability::ActiveAbilities,
    ability_pool: &comp::ability::AbilityPool,
) -> Vec<DatabaseAbilitySet> {
    active_abilities
        .auxiliary_sets
        .iter()
        .map(|((mainhand, offhand), abilities)| DatabaseAbilitySet {
            mainhand: tool_kind_to_string(*mainhand),
            offhand: tool_kind_to_string(*offhand),
            abilities: abilities
                .iter()
                .map(|ability| aux_ability_to_string(*ability, ability_pool))
                .collect(),
        })
        .collect::<Vec<_>>()
}

pub fn active_abilities_from_db_model(
    ability_sets: Vec<DatabaseAbilitySet>,
    ability_pool: &comp::ability::AbilityPool,
) -> comp::ability::ActiveAbilities {
    let ability_sets = ability_sets
        .into_iter()
        .map(
            |DatabaseAbilitySet {
                 mainhand,
                 offhand,
                 abilities,
             }| {
                let mut auxiliary_abilities =
                    vec![comp::ability::AuxiliaryAbility::Empty; comp::ability::BASE_ABILITY_LIMIT];
                for (empty, ability) in auxiliary_abilities.iter_mut().zip(abilities) {
                    *empty = aux_ability_from_string(&ability, ability_pool);
                }
                (
                    (
                        tool_kind_from_string(mainhand),
                        tool_kind_from_string(offhand),
                    ),
                    auxiliary_abilities,
                )
            },
        )
        .collect::<HashMap<_, _>>();
    comp::ability::ActiveAbilities::from_auxiliary(
        ability_sets,
        Some(comp::ability::BASE_ABILITY_LIMIT),
    )
}

/// Struct containing item properties in the format that they get persisted to
/// the database. Adding new fields is generally safe as long as they are
/// optional. Renaming or removing old fields will require a migration.
#[derive(Serialize, Deserialize)]
pub struct DatabaseItemProperties {
    #[serde(skip_serializing_if = "Option::is_none")]
    durability: Option<NonZeroU32>,
}

pub fn item_properties_to_db_model(item: &comp::Item) -> DatabaseItemProperties {
    DatabaseItemProperties {
        durability: item.persistence_durability(),
    }
}

pub fn apply_db_item_properties(item: &mut comp::Item, properties: &DatabaseItemProperties) {
    let DatabaseItemProperties { durability } = properties;
    item.persistence_set_durability(*durability);
}

#[cfg(test)]
pub mod tests {
    /// BL-04: every `ClassKind` must survive both persistence converters in
    /// both directions, so a new variant can't silently drop from one (the
    /// guard the `ClassKind::ALL` doc-comment promises).
    #[test]
    fn class_persistence_round_trips_for_every_class() {
        use common::comp::{class::ClassKind, skillset::SkillGroupKind};
        for class in ClassKind::ALL {
            // CharacterClass converter (both directions).
            assert_eq!(
                super::db_string_to_class(&super::class_to_db_string(class)),
                class,
                "class_to_db_string round-trip failed for {class:?}"
            );
            // Skill-group converter (both directions) — Adventurer has no class
            // tree and intentionally panics, so skip it.
            if class != ClassKind::Adventurer {
                let group = SkillGroupKind::Class(class);
                assert_eq!(
                    super::db_string_to_skill_group(&super::skill_group_to_db_string(group)),
                    group,
                    "skill_group_to_db_string round-trip failed for {class:?}"
                );
            }
        }
    }

    #[test]
    fn test_default_item_properties() {
        use super::DatabaseItemProperties;
        const DEFAULT_ITEM_PROPERTIES: &str = "{}";
        let _ = serde_json::de::from_str::<DatabaseItemProperties>(DEFAULT_ITEM_PROPERTIES).expect(
            "Default value should always load to ensure that changes to item properties is always \
             forward compatible with migration V50.",
        );
    }

    #[test]
    fn skill_group_db_string_round_trips() {
        use common::comp::{class::ClassKind, item::tool::ToolKind, skillset::SkillGroupKind};
        let kinds = [
            SkillGroupKind::General,
            SkillGroupKind::Weapon(ToolKind::Sword),
            SkillGroupKind::Weapon(ToolKind::Axe),
            SkillGroupKind::Weapon(ToolKind::Hammer),
            SkillGroupKind::Weapon(ToolKind::Bow),
            SkillGroupKind::Weapon(ToolKind::Staff),
            SkillGroupKind::Weapon(ToolKind::Sceptre),
            SkillGroupKind::Weapon(ToolKind::Pick),
            SkillGroupKind::Class(ClassKind::Warrior),
            SkillGroupKind::Class(ClassKind::Mage),
            SkillGroupKind::Class(ClassKind::Cleric),
            SkillGroupKind::Class(ClassKind::Rogue),
        ];
        for kind in kinds {
            assert_eq!(
                super::db_string_to_skill_group(&super::skill_group_to_db_string(kind)),
                kind,
                "round trip failed for {kind:?}"
            );
        }
    }

    #[test]
    fn class_db_string_round_trips_and_tolerates_unknown() {
        use common::comp::class::ClassKind;
        for class in ClassKind::ALL {
            assert_eq!(
                super::db_string_to_class(&super::class_to_db_string(class)),
                class
            );
        }
        // A downgrade/foreign DB must never brick the server (spec §4)
        assert_eq!(
            super::db_string_to_class("Necromancer"),
            ClassKind::Adventurer
        );
    }

    /// BL-31 task BG1b.5 (updated post-`Custom` removal): every
    /// `BackgroundKind` round-trips through its db string; an unrecognized
    /// string degrades to `None` (never panics — P0 §Q1, a DB downgrade must
    /// never brick the server, mirroring the class-string test above).
    #[test]
    fn background_db_string_round_trips_and_tolerates_unknown() {
        use common::comp::BackgroundKind;
        for background in BackgroundKind::ALL {
            assert_eq!(
                super::db_string_to_background(&super::background_to_db_string(background)),
                Some(background),
                "{background:?} did not round-trip through its db string"
            );
        }
        assert_eq!(super::db_string_to_background("necromancer"), None);
        // The removed "custom" value degrades to `None` like any other
        // unrecognized string, rather than panicking.
        assert_eq!(super::db_string_to_background("custom"), None);
    }

    /// A synthetic pool, built by hand rather than through
    /// `AbilityPool::for_character`, so these tests neither load assets nor
    /// depend on what any particular class currently grants.
    fn test_pool(keys: &[&str]) -> common::comp::ability::AbilityPool {
        common::comp::ability::AbilityPool {
            abilities: keys.iter().map(|k| (*k).to_string()).collect(),
            spell_gates: vec![None; keys.len()],
        }
    }

    #[test]
    fn innate_aux_ability_round_trips() {
        use common::comp::ability::AuxiliaryAbility;
        let pool = test_pool(&[
            "class.mage.arcane_bolt",
            "innate.human",
            "spells.evocation.shatterburst",
            "spells.abjuration.ward",
        ]);
        for ability in [
            AuxiliaryAbility::Innate(0),
            AuxiliaryAbility::Innate(3),
            AuxiliaryAbility::MainWeapon(1),
            AuxiliaryAbility::Empty,
        ] {
            let s = super::aux_ability_to_string(ability, &pool);
            assert_eq!(super::aux_ability_from_string(&s, &pool), ability);
        }
        // Innate slots are now written by key, not by position.
        assert_eq!(
            super::aux_ability_to_string(AuxiliaryAbility::Innate(3), &pool),
            "Innate:key:spells.abjuration.ward"
        );
    }

    /// Back-compat guarantee: rows written before the key format still resolve
    /// exactly as they did, so no DB migration is needed.
    #[test]
    fn legacy_index_form_still_parses() {
        use common::comp::ability::AuxiliaryAbility;
        let pool = test_pool(&[
            "class.mage.arcane_bolt",
            "innate.human",
            "spells.evocation.shatterburst",
            "spells.abjuration.ward",
        ]);
        assert_eq!(
            super::aux_ability_from_string("Innate:index:3", &pool),
            AuxiliaryAbility::Innate(3)
        );
        // Even an index the current pool cannot back — the resolution is
        // positional and unchanged from before.
        assert_eq!(
            super::aux_ability_from_string("Innate:index:9", &pool),
            AuxiliaryAbility::Innate(9)
        );
    }

    #[test]
    fn key_form_round_trips() {
        use common::comp::ability::AuxiliaryAbility;
        let pool = test_pool(&[
            "class.mage.arcane_bolt",
            "innate.human",
            "spells.evocation.shatterburst",
            "spells.abjuration.ward",
            "spells.necromancy.wither",
            "spells.divination.foresight",
        ]);
        let ability = AuxiliaryAbility::Innate(5);
        let s = super::aux_ability_to_string(ability, &pool);
        assert_eq!(s, format!("Innate:key:{}", pool.abilities[5]));
        assert_eq!(super::aux_ability_from_string(&s, &pool), ability);
    }

    /// A removed spell, or a class the character no longer holds, is a
    /// legitimate content change — the slot clears, and must NOT `dev_panic!`
    /// (which, under `debug_assertions`, would fail this test).
    #[test]
    fn a_key_that_left_the_pool_clears_the_slot_quietly() {
        use common::comp::ability::AuxiliaryAbility;
        let warrior_pool = test_pool(&["class.warrior.rally", "innate.human"]);
        assert_eq!(
            super::aux_ability_from_string(
                "Innate:key:spells.evocation.shatterburst",
                &warrior_pool
            ),
            AuxiliaryAbility::Empty,
        );
        // Including against a pool with nothing in it at all.
        assert_eq!(
            super::aux_ability_from_string(
                "Innate:key:spells.evocation.shatterburst",
                &test_pool(&[])
            ),
            AuxiliaryAbility::Empty,
        );
    }

    /// The whole point of the key format: growing the compendium re-numbers the
    /// pool, and a bound slot must follow its spell rather than its old
    /// position.
    #[test]
    fn adding_spells_to_the_compendium_no_longer_moves_a_bound_slot() {
        use common::comp::ability::AuxiliaryAbility;
        let before = test_pool(&[
            "class.mage.arcane_bolt",
            "spells.evocation.shatterburst",
            "spells.abjuration.ward",
        ]);
        // The same pool after a content patch inserted one spell in the middle.
        let after = test_pool(&[
            "class.mage.arcane_bolt",
            "spells.conjuration.summon_imp",
            "spells.evocation.shatterburst",
            "spells.abjuration.ward",
        ]);

        // The player had `shatterburst` on the bar: index 1 before, 2 after.
        let bound = AuxiliaryAbility::Innate(1);
        let persisted = super::aux_ability_to_string(bound, &before);
        assert_eq!(persisted, "Innate:key:spells.evocation.shatterburst");

        let reloaded = super::aux_ability_from_string(&persisted, &after);
        assert_eq!(reloaded, AuxiliaryAbility::Innate(2));
        // …and it still names the same spell on the next save.
        assert_eq!(
            super::aux_ability_to_string(reloaded, &after),
            persisted,
            "the slot must round-trip to the same key across the insertion"
        );

        // Contrast with what the old positional format would have done: index 1
        // in the patched pool is the newly inserted spell, not the bound one.
        assert_eq!(after.abilities[1], "spells.conjuration.summon_imp");
    }

    /// Defensive only — `Innate(i)` should never point past the pool. If it
    /// somehow does, fall back to the legacy positional form rather than
    /// silently dropping the slot.
    #[test]
    fn an_out_of_range_innate_index_falls_back_to_the_legacy_form() {
        use common::comp::ability::AuxiliaryAbility;
        let pool = test_pool(&["class.mage.arcane_bolt"]);
        assert_eq!(
            super::aux_ability_to_string(AuxiliaryAbility::Innate(7), &pool),
            "Innate:index:7"
        );
    }

    mod trigger_slots {
        use chrono::{DateTime, TimeDelta, Utc};
        use common::{
            comp::{
                SlotState, TriggerCondition, TriggerSlot, TriggerSlots, ability::AuxiliaryAbility,
                trigger::MAX_TRIGGER_SLOTS,
            },
            resources::Time,
        };

        fn instant() -> DateTime<Utc> { DateTime::from_timestamp(1_700_000_000, 0).unwrap() }

        #[test]
        fn a_character_with_no_triggers_writes_no_column() {
            assert_eq!(
                super::super::trigger_slots_to_db_string(&TriggerSlots::default()),
                None
            );
        }

        #[test]
        fn a_null_column_loads_as_nothing_configured() {
            let loaded = super::super::db_string_to_trigger_slots(None);
            assert!(!loaded.has_any_configured());
        }

        /// 🔴 The whole reason this column exists: a slot cooling for
        /// thirty-six hours must still be cooling after a relog, with the
        /// remaining wait intact.
        #[test]
        fn a_running_cooldown_survives_the_round_trip_with_its_remaining_wait() {
            let ready_at = instant() + TimeDelta::hours(36);
            let mut before = TriggerSlots::default();
            before.slots[2] = Some(TriggerSlot {
                ability: AuxiliaryAbility::Innate(3),
                condition: TriggerCondition::HealthBelow(0.25),
                state: SlotState::CoolingDown {
                    ready_at: Some(ready_at),
                    ready_at_time: Time(999.0),
                },
            });

            let column = super::super::trigger_slots_to_db_string(&before).expect("a column");
            let mut after = super::super::db_string_to_trigger_slots(Some(&column));

            assert_eq!(
                after.get(2).map(|s| s.ability),
                Some(AuxiliaryAbility::Innate(3))
            );
            assert_eq!(
                after.get(2).map(|s| s.condition),
                Some(TriggerCondition::HealthBelow(0.25)),
            );
            assert_eq!(
                after.get(2).and_then(|s| s.state.ready_at()),
                Some(ready_at)
            );
            // Nothing is ready until the projection is rebuilt from the clock.
            assert_eq!(
                after.get(2).and_then(|s| s.state.ready_at_time()),
                Some(Time(f64::INFINITY)),
            );

            // One hour of real time passed while logged out: 35 hours left.
            after.reproject_cooldowns(instant() + TimeDelta::hours(1), Time(50.0));
            assert_eq!(
                after.get(2).and_then(|s| s.state.ready_at_time()),
                Some(Time(50.0 + 35.0 * 3600.0)),
            );
        }

        /// A slot that was mid-cast when the save happened comes back ready,
        /// never holding an authorisation token no cast will ever claim.
        #[test]
        fn a_firing_slot_is_never_persisted_as_firing() {
            let mut before = TriggerSlots::default();
            before.slots[0] = Some(TriggerSlot {
                ability: AuxiliaryAbility::Innate(0),
                condition: TriggerCondition::DamageTaken,
                state: SlotState::Firing {
                    ability_id: "innate.danari".to_string(),
                    deadline: Time(5.0),
                },
            });

            let column = super::super::trigger_slots_to_db_string(&before).expect("a column");
            let after = super::super::db_string_to_trigger_slots(Some(&column));
            assert_eq!(
                after.get(0).map(|s| s.state.clone()),
                Some(SlotState::Ready)
            );
            assert_eq!(after.firing_token(0), None);
        }

        #[test]
        fn every_slot_index_round_trips_independently() {
            let mut before = TriggerSlots::default();
            for index in 0..MAX_TRIGGER_SLOTS {
                before.slots[index] = Some(TriggerSlot::new(
                    AuxiliaryAbility::Innate(index),
                    TriggerCondition::EnergyBelow(0.1 * index as f32),
                ));
            }
            let column = super::super::trigger_slots_to_db_string(&before).expect("a column");
            let after = super::super::db_string_to_trigger_slots(Some(&column));
            for index in 0..MAX_TRIGGER_SLOTS {
                assert_eq!(
                    after.configured_ability(index),
                    Some(AuxiliaryAbility::Innate(index)),
                    "slot {index}"
                );
            }
        }

        /// A corrupt payload must never lock a character out of the game.
        #[test]
        fn an_unreadable_column_loads_as_nothing_configured() {
            let loaded = super::super::db_string_to_trigger_slots(Some("{not json"));
            assert!(!loaded.has_any_configured());
        }
    }
}
