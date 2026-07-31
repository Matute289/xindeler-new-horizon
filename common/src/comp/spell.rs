//! Spell compendium metadata (magic-system-v2 spec §3). Pure UI/gating layer:
//! every `SpellDef` points at a `CharacterAbility` RON that actually executes.
//! Combat reads the ability; spellbook UI, class gating, and tooltips read
//! this.
use crate::{
    assets::{Asset, AssetCache, AssetExt, AssetReadGuard, BoxedError, Ron, SharedString},
    comp::{
        ability::{MagicSource, School},
        class::{CharacterClass, ClassKind},
    },
};
use hashbrown::HashMap;
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub enum CastTime {
    Action,
    Bonus,
    Reaction,
    Minutes(u8),
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub enum SpellDuration {
    Instant,
    Secs(f32),
    Concentration(f32),
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub enum SpellRange {
    SelfOnly,
    Touch,
    Meters(f32),
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub enum SpellAoe {
    Sphere(f32),
    Cone(f32),
    Line(f32),
    Cube(f32),
}

/// One catalogued spell. Metadata only; `ability` is the asset specifier of the
/// `CharacterAbility` RON that runs when cast.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SpellDef {
    pub id: String,
    pub name_i18n: String,
    /// 0 = cantrip … 9.
    pub level: u8,
    pub school: Option<School>,
    pub source: MagicSource,
    pub classes: Vec<ClassKind>,
    pub cast_time: CastTime,
    pub duration: SpellDuration,
    pub range: SpellRange,
    pub aoe: Option<SpellAoe>,
    pub description_i18n: String,
    /// Asset path of the executing `CharacterAbility` RON.
    pub ability: String,
}

#[derive(Clone, Debug, Default)]
pub struct SpellCompendium {
    pub defs: Vec<SpellDef>,
    /// `SpellDef.ability` (the executing ability's asset specifier) → that
    /// spell's `classes` list. Built once here, at asset-load time, so the
    /// cast-time per-spell filter (`Self::allows`) never linear-scans `defs`
    /// per activation.
    by_ability: HashMap<String, Vec<ClassKind>>,
}

impl Asset for SpellCompendium {
    fn load(cache: &AssetCache, specifier: &SharedString) -> Result<Self, BoxedError> {
        let defs = cache
            .load::<Ron<Vec<SpellDef>>>(specifier)?
            .read()
            .0
            .clone();
        let by_ability = defs
            .iter()
            .map(|def| (def.ability.clone(), def.classes.clone()))
            .collect();
        Ok(SpellCompendium { defs, by_ability })
    }
}

impl SpellCompendium {
    pub fn load_expect_cloned() -> Self { Self::load_expect("common.spells.compendium").cloned() }

    /// The cast-time per-spell class filter (magic-system-v2 spec §7),
    /// applied below the class-vs-source core gate
    /// (`Stats::can_cast`/`states::utils::handle_ability`). Four cases pass
    /// unconditionally, each backing a specific shipped design:
    /// - `ability_id` has no compendium entry — most abilities aren't
    ///   catalogued; absence is not denial.
    /// - `character_class` is `None` — an entity with no `CharacterClass`
    ///   (every NPC, summon and boss) keeps casting, same rule as the core
    ///   gate.
    /// - the caster holds `ClassKind::Adventurer` — the legacy pre-class
    ///   value; most catalogued entries only list `Mage`, so gating
    ///   Adventurer would strip every legacy character's kit.
    /// - (checked by the caller, not here) `Ability::InnateAux` — an innate
    ///   grant bypasses this per-spell list the same way it bypasses the
    ///   core gate, since it is how classes with no catalogued entry of
    ///   their own (or an empty core mask) receive their signature spells.
    ///
    /// Otherwise passes when any class the caster holds
    /// (`CharacterClass::classes`) appears in the entry's `classes` list —
    /// a multiclass character needs only one of its two classes to match.
    pub fn allows(&self, ability_id: &str, character_class: Option<&CharacterClass>) -> bool {
        let Some(allowed_classes) = self.by_ability.get(ability_id) else {
            return true;
        };
        let Some(character_class) = character_class else {
            return true;
        };
        character_class
            .classes()
            .any(|class| class == ClassKind::Adventurer || allowed_classes.contains(&class))
    }
}

/// One cache read for per-activation consumers — an `AssetReadGuard`, not a
/// clone, so the cast-time per-spell filter can read the catalogue once per
/// ability activation without cloning `defs` or rebuilding `by_ability`
/// (mirrors `class::class_magic_sources_manifest`'s own doc comment).
pub fn spell_compendium_manifest() -> AssetReadGuard<SpellCompendium> {
    SpellCompendium::load_expect("common.spells.compendium").read()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{assets::AssetExt, comp::ability::CharacterAbility};

    #[test]
    fn compendium_loads_and_abilities_resolve() {
        let book = SpellCompendium::load_expect_cloned();
        assert!(!book.defs.is_empty(), "compendium is empty");
        for spell in &book.defs {
            Ron::<CharacterAbility>::load_expect(&spell.ability).read();
        }
    }

    fn compendium_of(entries: &[(&str, &[ClassKind])]) -> SpellCompendium {
        SpellCompendium {
            defs: Vec::new(),
            by_ability: entries
                .iter()
                .map(|(id, classes)| ((*id).to_string(), classes.to_vec()))
                .collect(),
        }
    }

    #[test]
    fn allows_uncatalogued_ability_passes() {
        let book = compendium_of(&[("common.abilities.spells.ruin.shatterburst", &[
            ClassKind::Mage,
        ])]);
        assert!(book.allows(
            "common.abilities.staff.firebomb",
            Some(&CharacterClass::single(ClassKind::Warrior))
        ));
    }

    #[test]
    fn allows_no_character_class_passes() {
        let book = compendium_of(&[("spell.a", &[ClassKind::Mage])]);
        assert!(book.allows("spell.a", None));
    }

    #[test]
    fn allows_adventurer_passes_everything() {
        let book = compendium_of(&[("spell.a", &[ClassKind::Mage])]);
        assert!(book.allows(
            "spell.a",
            Some(&CharacterClass::single(ClassKind::Adventurer))
        ));
    }

    #[test]
    fn allows_matches_the_catalogued_class() {
        let book = compendium_of(&[
            ("spell.mage", &[ClassKind::Mage]),
            ("spell.cleric", &[ClassKind::Cleric]),
        ]);
        assert!(book.allows("spell.mage", Some(&CharacterClass::single(ClassKind::Mage))));
        assert!(!book.allows(
            "spell.mage",
            Some(&CharacterClass::single(ClassKind::Cleric))
        ));
        assert!(book.allows(
            "spell.cleric",
            Some(&CharacterClass::single(ClassKind::Cleric))
        ));
    }

    #[test]
    fn allows_multiclass_passes_if_either_class_matches() {
        let book = compendium_of(&[
            ("spell.mage", &[ClassKind::Mage]),
            ("spell.cleric", &[ClassKind::Cleric]),
        ]);
        let multiclass = CharacterClass {
            primary: ClassKind::Cleric,
            secondary: Some(ClassKind::Mage),
            secondary_level: 5,
            future_levels_to_secondary: false,
        };
        assert!(book.allows("spell.mage", Some(&multiclass)));
        assert!(book.allows("spell.cleric", Some(&multiclass)));
    }
}
