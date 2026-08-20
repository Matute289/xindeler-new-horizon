use crate::persistence::{PersistedComponents, character_updater::CharacterUpdater};
use common::{
    character::CharacterId,
    comp::{
        BASE_ABILITY_LIMIT, Background, Body, CharacterClass, Content, Inventory, Item, SkillSet,
        Stats, Waypoint, class::ClassKind, inventory::loadout_builder::LoadoutBuilder,
        skillset::SkillGroupKind,
    },
};
use specs::{Entity, WriteExpect};

/// BL-31 P1 stub (task BG1c.1): background kit items are content (P3,
/// `background_features.ron` + per-background kit RONs), which does not
/// exist yet. This hook is wired now so P3 only has to fill in the body —
/// it never touches call sites again. Until then this is a no-op: the
/// `LoadoutBuilder` is returned unchanged regardless of `background`.
fn apply_background_kit(
    _background: &Background,
    loadout_builder: LoadoutBuilder,
) -> LoadoutBuilder {
    // TODO(BL-31 P3): look up `background.0`'s kit path in
    // `assets/common/backgrounds/background_features.ron` and, if `Some`,
    // `loadout_builder.with_asset_expect(kit_path, &mut rng, None)`.
    loadout_builder
}

/// Per-class starter weapon whitelist (spec §3/§5). `[None, None]` is always
/// accepted separately for unmodified clients.
fn valid_starter_items(class: ClassKind) -> &'static [[Option<&'static str>; 2]] {
    match class {
        ClassKind::Adventurer => &[],
        ClassKind::Warrior => &[
            [Some("common.items.weapons.sword.starter"), None],
            [Some("common.items.weapons.axe.starter_axe"), None],
            [Some("common.items.weapons.hammer.starter_hammer"), None],
        ],
        // Mage's kit is a plain, unbuffed Tome -- explicitly no staff. The
        // Tome's own equip gate lists only Mage.
        ClassKind::Mage => &[[Some("common.items.weapons.tome.apprentice_tome"), None]],
        // Cleric picks between a Sceptre or a Holy Symbol at creation, both
        // plain/unbuffed tier.
        ClassKind::Cleric => &[
            [Some("common.items.weapons.sceptre.starter_sceptre"), None],
            [
                Some("common.items.weapons.holy_symbol.initiate_symbol"),
                None,
            ],
        ],
        ClassKind::Rogue => &[
            [
                Some("common.items.weapons.sword_1h.starter"),
                Some("common.items.weapons.sword_1h.starter"),
            ],
            [Some("common.items.weapons.bow.starter"), None],
        ],
        // Valid existing starters by archetype; thematic implements beyond
        // the ones already assigned are a later content pass.
        ClassKind::Barbarian => &[[Some("common.items.weapons.axe.starter_axe"), None], [
            Some("common.items.weapons.hammer.starter_hammer"),
            None,
        ]],
        // Sorcerer and Warlock cast their spell-slot kits with no implement
        // equipped at all -- `AbilityPool::for_character` embeds
        // `spells_for_class` unconditionally, so pool spells cast fine with
        // nothing in hand. Their only starter "kit" is empty-handed;
        // whatever they later equip is a pure stat buff, never a casting
        // requirement.
        ClassKind::Sorcerer | ClassKind::Warlock => &[[None, None]],
        // Druid picks between a Staff, a Sceptre, or a Focus at creation,
        // all plain/unbuffed tier.
        ClassKind::Druid => &[
            [Some("common.items.weapons.staff.starter_staff"), None],
            [Some("common.items.weapons.sceptre.starter_sceptre"), None],
            [Some("common.items.weapons.focus.primordial_focus"), None],
        ],
        // Artificer was previously lumped in with the staff-starting classes
        // above, but `starter_staff`'s own equip gate (its `requirements:`
        // block / `equip_gates.ron`'s `(Staff, Caster)` row) only lists
        // Mage/Sorcerer/Warlock/Druid — never Artificer. Artificer's own
        // `class_proficiencies.ron` entry is `Any(Hammer)`, so hand out the
        // (ungated, martial) Hammer instead, matching every other class's
        // own proficiency.
        ClassKind::Artificer => &[[Some("common.items.weapons.hammer.starter_hammer"), None]],
        // The Bard starts with a musical instrument, not a mage's staff.
        // `starter_staff`'s own `requirements:` block doesn't list Bard
        // (only Mage/Sorcerer/Warlock/Druid), so handing it out here would
        // give a class a starter item that fails that same item's own
        // equip gate. Instrument items carry no `requirements:` block at
        // all (see class_proficiencies.ron's Bard comment) — equipping and
        // playing one is open to every class; only casting spells through
        // an instrument is meant to stay Bard-only, and that mechanism
        // does not exist in the ability-set data yet.
        ClassKind::Bard => &[[Some("common.items.tool.instruments.lute"), None]],
        ClassKind::Paladin | ClassKind::BloodSlayer => {
            &[[Some("common.items.weapons.sword.starter"), None]]
        },
        ClassKind::Ranger => &[[Some("common.items.weapons.bow.starter"), None]],
        ClassKind::Monk => &[[Some("common.items.weapons.sword_1h.starter"), None]],
    }
}

/// One flavorful consumable per class (all verified under
/// assets/common/items/consumable/).
fn class_kit_item(class: ClassKind) -> &'static str {
    match class {
        ClassKind::Adventurer | ClassKind::Warrior => "common.items.consumable.potion_minor",
        ClassKind::Mage | ClassKind::Rogue => "common.items.consumable.potion_agility",
        ClassKind::Cleric => "common.items.consumable.potion_med",
        // Classes-wave (BL-04).
        ClassKind::Sorcerer
        | ClassKind::Warlock
        | ClassKind::Bard
        | ClassKind::Druid
        | ClassKind::Artificer => "common.items.consumable.potion_minor",
        ClassKind::Ranger | ClassKind::Monk => "common.items.consumable.potion_agility",
        ClassKind::Barbarian | ClassKind::Paladin | ClassKind::BloodSlayer => {
            "common.items.consumable.potion_med"
        },
    }
}

// Upstream names the variants InvalidWeapon/InvalidBody; keeping the prefix
// for the added InvalidClass minimizes the upstream-merge surface (renaming
// would touch every call site). Three same-prefix variants trip the lint.
#[expect(clippy::enum_variant_names)]
#[derive(Debug)]
pub enum CreationError {
    InvalidWeapon,
    InvalidBody,
    InvalidClass,
}

pub fn create_character(
    entity: Entity,
    player_uuid: String,
    character_alias: String,
    character_mainhand: Option<String>,
    character_offhand: Option<String>,
    body: Body,
    character_class: ClassKind,
    ethos: common::comp::Ethos,
    background: Background,
    hardcore: bool,
    character_updater: &mut WriteExpect<'_, CharacterUpdater>,
    waypoint: Option<Waypoint>,
) -> Result<(), CreationError> {
    // quick fix whitelist validation for now; eventually replace the
    // `Option<String>` with an index into a server-provided list of starter
    // items, and replace `comp::body::Body` with `comp::body::humanoid::Body`
    // throughout the messages involved
    if !matches!(body, Body::Humanoid(_)) {
        return Err(CreationError::InvalidBody);
    }
    if !character_class.is_playable() {
        return Err(CreationError::InvalidClass);
    }
    // [None, None] (no weapons) bypasses the class whitelist on purpose — stock
    // clients may create without weapons (zesterer); guard structure preserves it.
    if !(character_mainhand.is_none() && character_offhand.is_none())
        && !valid_starter_items(character_class)
            .contains(&[character_mainhand.as_deref(), character_offhand.as_deref()])
    {
        return Err(CreationError::InvalidWeapon);
    };
    // The client sends None if a weapon hand is empty
    let mut rng = rand::rng();
    let loadout_builder = LoadoutBuilder::empty().defaults().with_asset_expect(
        &format!("common.loadout.class.{}", character_class.keyword()),
        &mut rng,
        None,
    );
    // BL-31 P1 (task BG1c.1): background-kit grant stub, applied after the
    // class loadout. No-op until P3 provides `background_features.ron`.
    let loadout_builder = apply_background_kit(&background, loadout_builder);
    let loadout = loadout_builder
        .active_mainhand(character_mainhand.map(|x| Item::new_from_asset_expect(&x)))
        .active_offhand(character_offhand.map(|x| Item::new_from_asset_expect(&x)))
        .build();
    let mut inventory = Inventory::with_loadout_humanoid(loadout);

    let stats = Stats::new(Content::Plain(character_alias.to_string()), body);
    let mut skill_set = SkillSet::default();
    skill_set.unlock_skill_group(SkillGroupKind::Class(character_class));
    // Default items for new characters
    inventory
        .push(Item::new_from_asset_expect(
            "common.items.consumable.potion_minor",
        ))
        .expect("Inventory has at least 2 slots left!");
    inventory
        .push(Item::new_from_asset_expect("common.items.food.cheese"))
        .expect("Inventory has at least 1 slot left!");
    inventory
        .push_recipe_group(Item::new_from_asset_expect("common.items.recipes.default"))
        .expect("New inventory should not already have default recipe group.");
    inventory
        .push(Item::new_from_asset_expect(class_kit_item(character_class)))
        .expect("Inventory has at least 1 slot left!");

    let map_marker = None;

    character_updater.create_character(entity, player_uuid, character_alias, PersistedComponents {
        body,
        hardcore: hardcore.then_some(common::comp::Hardcore),
        character_class: CharacterClass::single(character_class),
        stats,
        skill_set,
        inventory,
        waypoint,
        pets: Vec::new(),
        active_abilities: common::comp::ActiveAbilities::default_limited(BASE_ABILITY_LIMIT),
        map_marker,
        // BL-33: the alignment chosen at character creation (defaults to True
        // Neutral if the client sends it). Sanitised — never trust the wire
        // value. Deeds then drift it in-game (P3).
        ethos: ethos.clamped(),
        // BL-31: the background chosen at character creation, or
        // `Background(None)` ("Uncommitted", P0 §Q1).
        background,
        // A pact is bound in-game (`/pact bind`), never chosen at creation.
        pact: common::comp::Pact::default(),
        // Trigger slots are configured in-game, never at creation.
        trigger_slots: common::comp::TriggerSlots::default(),
        // Mastery accrues in-game, never at creation.
        spell_mastery: common::comp::SpellMastery::default(),
    });
    Ok(())
}

pub fn edit_character(
    entity: Entity,
    player_uuid: String,
    id: CharacterId,
    character_alias: String,
    body: Body,
    character_updater: &mut WriteExpect<'_, CharacterUpdater>,
) -> Result<(), CreationError> {
    if !matches!(body, Body::Humanoid(_)) {
        return Err(CreationError::InvalidBody);
    }

    character_updater.edit_character(
        entity,
        player_uuid,
        id,
        Some(character_alias),
        (body,),
        None,
    );
    Ok(())
}

// Error handling
impl core::fmt::Display for CreationError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            CreationError::InvalidWeapon => write!(
                f,
                "Invalid weapon.\nServer and client might be partially incompatible."
            ),
            CreationError::InvalidBody => write!(
                f,
                "Invalid Body.\nServer and client might be partially incompatible"
            ),
            CreationError::InvalidClass => write!(
                f,
                "Invalid class.\nServer and client might be partially incompatible."
            ),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use common::comp::class::ClassKind;

    #[test]
    fn every_class_has_starter_weapons_and_they_load() {
        for class in ClassKind::PLAYABLE {
            let kits = valid_starter_items(class);
            assert!(!kits.is_empty(), "{class:?} has no starter weapons");
            for pair in kits {
                for item in pair.iter().flatten() {
                    Item::new_from_asset_expect(item);
                }
            }
        }
    }

    /// The Bard's starter weapon must be a musical instrument, not a staff —
    /// a Bard has no `Staff` proficiency in `class_proficiencies.ron`, and
    /// `starter_staff`'s own equip gate doesn't even list Bard.
    #[test]
    fn bard_starts_with_an_instrument_not_a_staff() {
        use common::comp::item::{ItemKind, ToolKind};

        let kits = valid_starter_items(ClassKind::Bard);
        assert!(
            !kits.is_empty(),
            "Bard has no starter items configured at all"
        );
        for pair in kits {
            for item_id in pair.iter().flatten() {
                assert!(
                    !item_id.contains("staff"),
                    "Bard's starter kit still hands out a staff: {item_id}"
                );
                let item = Item::new_from_asset_expect(item_id);
                assert!(
                    matches!(
                        &*item.kind(),
                        ItemKind::Tool(tool) if tool.kind == ToolKind::Instrument
                    ),
                    "Bard's starter item {item_id} is not a musical instrument"
                );
            }
        }
    }

    /// Instrument items must stay usable by every class — they carry no
    /// `requirements:` equip gate at all, unlike Tome/HolySymbol/Focus/
    /// Staff/Sceptre. Any class can pick one up and make music; only actual
    /// spellcasting through an instrument is meant to be Bard-only, and that
    /// is enforced (once it exists) at the ability level, not the equip
    /// level.
    #[test]
    fn bard_starter_instrument_has_no_class_equip_gate() {
        let item = Item::new_from_asset_expect("common.items.tool.instruments.lute");
        assert_eq!(
            item.requirements(),
            None,
            "instrument starter item must not carry a class equip-gate"
        );
    }

    /// A class's starter item must never fail that same class's own equip
    /// gate — otherwise a fresh character can spawn holding gear it could
    /// never legally re-equip after unequipping it. Covers all 14 playable
    /// classes with no exceptions.
    #[test]
    fn starter_items_pass_their_own_class_gate() {
        use common::comp::body::humanoid;

        let body = Body::Humanoid(humanoid::Body::random());
        let skill_set = SkillSet::default();
        for class in ClassKind::PLAYABLE {
            let character_class = CharacterClass::single(class);
            for pair in valid_starter_items(class) {
                for item_id in pair.iter().flatten() {
                    let item = Item::new_from_asset_expect(item_id);
                    assert!(
                        item.meets_requirements_with_class(
                            Some(&character_class),
                            &skill_set,
                            &body
                        ),
                        "{class:?}'s starter item {item_id} fails {class:?}'s own equip gate"
                    );
                }
            }
        }
    }

    /// Mage's only starter kit is a plain Tome, not a staff -- guards
    /// against a future edit accidentally re-adding `starter_staff` to this
    /// arm.
    #[test]
    fn mage_starts_with_only_a_tome_and_no_staff() {
        let kits = valid_starter_items(ClassKind::Mage);
        assert_eq!(kits, &[[
            Some("common.items.weapons.tome.apprentice_tome"),
            None
        ]]);
    }

    /// Sorcerer and Warlock cast their spell-slot kits with nothing
    /// equipped, so their only starter kit alternative is empty-handed.
    #[test]
    fn sorcerer_and_warlock_start_empty_handed() {
        for class in [ClassKind::Sorcerer, ClassKind::Warlock] {
            let kits = valid_starter_items(class);
            assert_eq!(kits, &[[None, None]], "{class:?} should start empty-handed");
        }
    }

    /// A class whose starter kit is empty-handed must still expose at least
    /// one pool spell it can cast at creation -- otherwise "no implement" is
    /// indistinguishable from "no spells".
    #[test]
    fn sorcerer_and_warlock_have_a_castable_pool_spell_with_no_implement() {
        use common::comp::spell::SpellCompendium;

        let compendium = SpellCompendium::load_expect_cloned();
        for class in [ClassKind::Sorcerer, ClassKind::Warlock] {
            assert!(
                !compendium.spells_for_class(class).is_empty(),
                "{class:?} starts with no implement but has no pool-eligible spell either"
            );
        }
    }

    /// Cleric's chargen choice is exactly Sceptre or Holy Symbol, both
    /// single-hand-slot kits (`initiate_symbol.ron` is `hands: Two`, so it
    /// can never pair with a shield in the same kit).
    #[test]
    fn cleric_offers_sceptre_or_holy_symbol_choice() {
        let kits = valid_starter_items(ClassKind::Cleric);
        assert_eq!(kits, &[
            [Some("common.items.weapons.sceptre.starter_sceptre"), None],
            [
                Some("common.items.weapons.holy_symbol.initiate_symbol"),
                None
            ],
        ]);
    }

    /// Druid's chargen choice is exactly Staff, Sceptre, or Focus.
    #[test]
    fn druid_offers_staff_sceptre_or_focus_choice() {
        let kits = valid_starter_items(ClassKind::Druid);
        assert_eq!(kits, &[
            [Some("common.items.weapons.staff.starter_staff"), None],
            [Some("common.items.weapons.sceptre.starter_sceptre"), None],
            [Some("common.items.weapons.focus.primordial_focus"), None],
        ]);
    }

    #[test]
    fn class_loadouts_and_kit_items_load() {
        let mut rng = rand::rng();
        for class in ClassKind::PLAYABLE {
            let _ = LoadoutBuilder::empty().defaults().with_asset_expect(
                &format!("common.loadout.class.{}", class.keyword()),
                &mut rng,
                None,
            );
            Item::new_from_asset_expect(class_kit_item(class));
        }
    }

    /// BL-31 task BG1c.2: the background-kit grant stub must not panic for
    /// any background, including `None`.
    #[test]
    fn apply_background_kit_stub_does_not_panic_for_any_background() {
        use common::comp::BackgroundKind;

        let mut rng = rand::rng();
        let backgrounds = std::iter::once(Background(None))
            .chain(BackgroundKind::ALL.into_iter().map(|k| Background(Some(k))));
        for background in backgrounds {
            let loadout_builder = LoadoutBuilder::empty().defaults().with_asset_expect(
                &format!("common.loadout.class.{}", ClassKind::Warrior.keyword()),
                &mut rng,
                None,
            );
            let _ = apply_background_kit(&background, loadout_builder);
        }
    }
}
