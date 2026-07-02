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
}

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

#[derive(Serialize, Deserialize)]
pub struct DatabaseAbilitySet {
    mainhand: String,
    offhand: String,
    abilities: Vec<String>,
}

fn aux_ability_to_string(ability: comp::ability::AuxiliaryAbility) -> String {
    use common::comp::ability::AuxiliaryAbility;
    match ability {
        AuxiliaryAbility::MainWeapon(index) => format!("Main Weapon:index:{}", index),
        AuxiliaryAbility::OffWeapon(index) => format!("Off Weapon:index:{}", index),
        AuxiliaryAbility::Glider(index) => format!("Glider:index:{}", index),
        AuxiliaryAbility::Innate(index) => format!("Innate:index:{}", index),
        AuxiliaryAbility::Empty => String::from("Empty"),
    }
}

fn aux_ability_from_string(ability: &str) -> comp::ability::AuxiliaryAbility {
    use common::comp::ability::AuxiliaryAbility;
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
) -> Vec<DatabaseAbilitySet> {
    active_abilities
        .auxiliary_sets
        .iter()
        .map(|((mainhand, offhand), abilities)| DatabaseAbilitySet {
            mainhand: tool_kind_to_string(*mainhand),
            offhand: tool_kind_to_string(*offhand),
            abilities: abilities
                .iter()
                .map(|ability| aux_ability_to_string(*ability))
                .collect(),
        })
        .collect::<Vec<_>>()
}

pub fn active_abilities_from_db_model(
    ability_sets: Vec<DatabaseAbilitySet>,
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
                    *empty = aux_ability_from_string(&ability);
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

    #[test]
    fn innate_aux_ability_round_trips() {
        use common::comp::ability::AuxiliaryAbility;
        for ability in [
            AuxiliaryAbility::Innate(0),
            AuxiliaryAbility::Innate(3),
            AuxiliaryAbility::MainWeapon(1),
            AuxiliaryAbility::Empty,
        ] {
            let s = super::aux_ability_to_string(ability);
            assert_eq!(super::aux_ability_from_string(&s), ability);
        }
        assert_eq!(
            super::aux_ability_to_string(AuxiliaryAbility::Innate(3)),
            "Innate:index:3"
        );
    }
}
