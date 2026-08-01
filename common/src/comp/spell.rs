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

/// Class levels required per spell level: a class unlocks the next spell level
/// every this many of its OWN levels.
pub const CLASS_LEVELS_PER_SPELL_LEVEL: u16 = 6;

/// The highest [`SpellDef::level`] a class of `class_level` has unlocked.
///
/// `floor(class_level / 6)`: class level 1-5 -> 0 (cantrips only), 6 -> 1, ...,
/// 54-59 -> 9, and exactly 60 (`MAX_CHARACTER_LEVEL`) -> 10, which denotes the
/// *possibility* of the capstone tier rather than an automatic unlock -- no
/// level-10 spell exists or is castable yet, so nothing consumes that value
/// today. Deliberately NOT clamped to 9, so that when the capstone tier is
/// designed the boundary is already where the design put it.
///
/// The input is a single class's own level (`CharacterClass::class_levels`),
/// never the raw character level: a multiclass character's caster side is
/// capped by how far that class itself has progressed.
pub fn spell_level_unlocked(class_level: u16) -> u16 { class_level / CLASS_LEVELS_PER_SPELL_LEVEL }

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

impl SpellDef {
    /// The `AbilityPool` / `AbilityMap` key for this spell. The compendium
    /// `id` is already in `"spells.<school>.<name>"` shape, so it doubles as
    /// the `AbilitySpec::Custom` key, the frontend ability id, and the i18n
    /// key stem -- exactly like the `class.*` and `innate.*` keys.
    pub fn pool_key(&self) -> &str { &self.id }
}

/// The catalogue of every authored spell, indexed by id.
///
/// Fields are private so the `id -> index` map can never drift out of sync
/// with the `Vec`; read it through [`Self::iter`], [`Self::get`] and
/// [`Self::spells_for_class`].
#[derive(Clone, Debug, Default)]
pub struct SpellCompendium {
    spells: Vec<SpellDef>,
    /// `id` -> index into `spells`, built once at asset load.
    by_id: HashMap<String, usize>,
    /// `ability` (the executing `CharacterAbility` asset specifier) -> index
    /// into `spells`, built once at asset load alongside `by_id`.
    ///
    /// A cast-time class check needs both keyings because `ability_id` (see
    /// `SpecifiedAbility::ability_id`) resolves differently depending on
    /// activation path: an `Ability::InnateAux` pool entry (this compendium's
    /// own `pool_key`/`id`, injected by `AbilityMap::load` as
    /// `AbilitySpec::Custom(id)`) yields `SpellDef.id`, while a spell wired
    /// directly into a weapon/implement ability set (e.g. a caster
    /// implement's `primary`/`secondary` in `ability_set_manifest.ron`,
    /// which never goes through the pool) yields the `AbilityItem.id`, i.e.
    /// `SpellDef.ability`. `Self::allows` checks `by_id` first, falling back
    /// to this index, so both paths are actually covered instead of the
    /// second one silently reading as "uncatalogued" every time.
    by_ability: HashMap<String, usize>,
}

impl Asset for SpellCompendium {
    fn load(cache: &AssetCache, specifier: &SharedString) -> Result<Self, BoxedError> {
        let spells = cache
            .load::<Ron<Vec<SpellDef>>>(specifier)?
            .read()
            .0
            .clone();
        let mut by_id = HashMap::with_capacity(spells.len());
        for (i, spell) in spells.iter().enumerate() {
            // A duplicate id would silently make one of the two entries
            // unreachable through `get`, so refuse to load at all.
            if by_id.insert(spell.id.clone(), i).is_some() {
                return Err(format!("duplicate spell id in compendium: {}", spell.id).into());
            }
        }
        // No duplicate-is-an-error rule here (unlike `by_id`): two spell
        // entries sharing one executing ability RON is unusual but not
        // inherently invalid, so the first entry wins rather than refusing
        // to load.
        let mut by_ability = HashMap::with_capacity(spells.len());
        for (i, spell) in spells.iter().enumerate() {
            by_ability.entry(spell.ability.clone()).or_insert(i);
        }
        Ok(SpellCompendium {
            spells,
            by_id,
            by_ability,
        })
    }
}

impl SpellCompendium {
    pub fn load_expect_cloned() -> Self { Self::load_expect("common.spells.compendium").cloned() }

    pub fn iter(&self) -> impl Iterator<Item = &SpellDef> { self.spells.iter() }

    pub fn get(&self, id: &str) -> Option<&SpellDef> {
        self.by_id.get(id).and_then(|i| self.spells.get(*i))
    }

    pub fn len(&self) -> usize { self.spells.len() }

    pub fn is_empty(&self) -> bool { self.spells.is_empty() }

    /// Every spell `class` can ever cast, in the canonical pool order:
    /// ascending `(level, id)`. Deterministic and independent of the order
    /// entries happen to appear in the RON, so re-sorting the asset file
    /// never changes a character's ability-pool indices.
    pub fn spells_for_class(&self, class: ClassKind) -> Vec<&SpellDef> {
        let mut out: Vec<&SpellDef> = self
            .spells
            .iter()
            .filter(|s| s.classes.contains(&class))
            .collect();
        out.sort_by(|a, b| (a.level, &a.id).cmp(&(b.level, &b.id)));
        out
    }

    /// `SpellDef.ability` -> the def, for the activation paths where
    /// `ability_id` yields the executing ability's asset specifier rather
    /// than the compendium id. See `by_ability`'s own doc comment.
    fn get_by_ability(&self, ability: &str) -> Option<&SpellDef> {
        self.by_ability
            .get(ability)
            .and_then(|&i| self.spells.get(i))
    }

    /// The cast-time per-spell class filter (magic-system-v2 spec §7),
    /// applied below the class-vs-source core gate
    /// (`Stats::can_cast`/`states::utils::handle_ability`). `ability_id` is
    /// whatever `SpecifiedAbility::ability_id` resolved for the activation —
    /// checked against both `by_id` (pool/`InnateAux` spells) and
    /// `by_ability` (a spell wired directly into a weapon/implement ability
    /// set), so this catches a catalogued spell regardless of which path
    /// delivered it.
    ///
    /// Three cases pass unconditionally, each backing a specific shipped
    /// design:
    /// - `ability_id` matches no compendium entry by either key — most
    ///   abilities aren't catalogued at all; absence is not denial.
    /// - `character_class` is `None` — an entity with no `CharacterClass`
    ///   (every NPC, summon and boss) keeps casting, same rule as the core
    ///   gate.
    /// - the caster holds `ClassKind::Adventurer` — the legacy pre-class value;
    ///   most catalogued entries only list `Mage`, so gating Adventurer would
    ///   strip every legacy character's kit.
    ///
    /// Otherwise passes when any class the caster holds
    /// (`CharacterClass::classes`) appears in the entry's `classes` list —
    /// a multiclass character needs only one of its two classes to match.
    ///
    /// Deliberately has no `Ability::InnateAux` exemption: every catalogued
    /// spell for a held class now reaches `handle_ability` as `InnateAux`
    /// (`AbilityPool::for_character` embeds the whole compendium per class),
    /// so exempting that variant here would exempt nearly the entire
    /// catalogue from this filter.
    pub fn allows(&self, ability_id: &str, character_class: Option<&CharacterClass>) -> bool {
        let Some(spell) = self
            .get(ability_id)
            .or_else(|| self.get_by_ability(ability_id))
        else {
            return true;
        };
        let Some(character_class) = character_class else {
            return true;
        };
        character_class
            .classes()
            .any(|class| class == ClassKind::Adventurer || spell.classes.contains(&class))
    }
}

/// One cache read for per-activation consumers — an `AssetReadGuard`, not a
/// clone, so the cast-time per-spell filter can read the catalogue once per
/// ability activation without cloning `spells` or rebuilding the `by_id`/
/// `by_ability` indices (mirrors `class::class_magic_sources_manifest`'s own
/// doc comment).
pub fn spell_compendium_manifest() -> AssetReadGuard<SpellCompendium> {
    SpellCompendium::load_expect("common.spells.compendium").read()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{assets::AssetExt, comp::ability::CharacterAbility};

    /// Every boundary row of the spell-level unlock table.
    #[test]
    fn unlock_table_matches_the_spec() {
        // 1-5: cantrips only.
        for lvl in 1..=5 {
            assert_eq!(spell_level_unlocked(lvl), 0, "class level {lvl}");
        }
        // 6 is the first level that unlocks spell level 1.
        assert_eq!(spell_level_unlocked(6), 1);
        assert_eq!(spell_level_unlocked(11), 1);
        assert_eq!(spell_level_unlocked(12), 2);
        assert_eq!(spell_level_unlocked(17), 2);
        assert_eq!(spell_level_unlocked(18), 3);
        assert_eq!(spell_level_unlocked(23), 3);
        assert_eq!(spell_level_unlocked(24), 4);
        assert_eq!(spell_level_unlocked(30), 5);
        assert_eq!(spell_level_unlocked(36), 6);
        assert_eq!(spell_level_unlocked(42), 7);
        assert_eq!(spell_level_unlocked(48), 8);
        // 54 is the first level with all nine normal spell levels.
        assert_eq!(spell_level_unlocked(54), 9);
        assert_eq!(spell_level_unlocked(59), 9);
        // 60 = MAX_CHARACTER_LEVEL produces 10: the *possibility* of the
        // capstone tier, deliberately unclamped.
        assert_eq!(spell_level_unlocked(60), 10);
    }

    #[test]
    fn level_zero_is_defensive_not_a_real_band() {
        // A class level of 0 should never occur, but must not panic or wrap.
        assert_eq!(spell_level_unlocked(0), 0);
    }

    #[test]
    fn compendium_lookup_by_id_is_exact() {
        let book = SpellCompendium::load_expect_cloned();
        assert!(!book.is_empty());
        for spell in book.iter() {
            assert_eq!(book.get(&spell.id).map(|s| &s.id), Some(&spell.id));
            // `pool_key` is the id itself -- the manifest/pool/i18n stem.
            assert_eq!(spell.pool_key(), spell.id.as_str());
        }
        assert!(book.get("spells.nope.not_a_spell").is_none());
        assert_eq!(book.len(), book.iter().count());
    }

    #[test]
    fn spells_for_class_is_sorted_and_filtered() {
        let book = SpellCompendium::load_expect_cloned();
        let mage = book.spells_for_class(ClassKind::Mage);
        assert!(!mage.is_empty(), "the compendium has Mage spells");
        assert!(mage.iter().all(|s| s.classes.contains(&ClassKind::Mage)));
        // Canonical order: ascending (level, id).
        let keys: Vec<_> = mage.iter().map(|s| (s.level, s.id.as_str())).collect();
        let mut sorted = keys.clone();
        sorted.sort();
        assert_eq!(keys, sorted, "spells_for_class must be (level, id)-sorted");
        // A class with no spells authored yet returns empty, not a panic.
        assert!(book.spells_for_class(ClassKind::Warrior).is_empty());
    }

    #[test]
    fn compendium_loads_and_abilities_resolve() {
        let book = SpellCompendium::load_expect_cloned();
        assert!(!book.is_empty(), "compendium is empty");
        for spell in book.iter() {
            Ron::<CharacterAbility>::load_expect(&spell.ability).read();
        }
    }

    fn test_spell(id: &str, ability: &str, classes: &[ClassKind]) -> SpellDef {
        SpellDef {
            id: id.to_string(),
            name_i18n: String::new(),
            level: 0,
            school: None,
            source: MagicSource::Arcane,
            classes: classes.to_vec(),
            cast_time: CastTime::Action,
            duration: SpellDuration::Instant,
            range: SpellRange::SelfOnly,
            aoe: None,
            description_i18n: String::new(),
            ability: ability.to_string(),
        }
    }

    fn compendium_of(defs: Vec<SpellDef>) -> SpellCompendium {
        let mut by_id = HashMap::with_capacity(defs.len());
        let mut by_ability = HashMap::with_capacity(defs.len());
        for (i, def) in defs.iter().enumerate() {
            by_id.insert(def.id.clone(), i);
            by_ability.entry(def.ability.clone()).or_insert(i);
        }
        SpellCompendium {
            spells: defs,
            by_id,
            by_ability,
        }
    }

    #[test]
    fn allows_uncatalogued_ability_passes() {
        let book = compendium_of(vec![test_spell(
            "spells.ruin.shatterburst",
            "common.abilities.spells.ruin.shatterburst",
            &[ClassKind::Mage],
        )]);
        assert!(book.allows(
            "common.abilities.staff.firebomb",
            Some(&CharacterClass::single(ClassKind::Warrior))
        ));
    }

    #[test]
    fn allows_no_character_class_passes() {
        let book = compendium_of(vec![test_spell("spells.a", "abilities.a", &[
            ClassKind::Mage,
        ])]);
        assert!(book.allows("spells.a", None));
    }

    #[test]
    fn allows_adventurer_passes_everything() {
        let book = compendium_of(vec![test_spell("spells.a", "abilities.a", &[
            ClassKind::Mage,
        ])]);
        assert!(book.allows(
            "spells.a",
            Some(&CharacterClass::single(ClassKind::Adventurer))
        ));
    }

    #[test]
    fn allows_matches_the_catalogued_class() {
        let book = compendium_of(vec![
            test_spell("spells.mage", "abilities.mage", &[ClassKind::Mage]),
            test_spell("spells.cleric", "abilities.cleric", &[ClassKind::Cleric]),
        ]);
        assert!(book.allows(
            "spells.mage",
            Some(&CharacterClass::single(ClassKind::Mage))
        ));
        assert!(!book.allows(
            "spells.mage",
            Some(&CharacterClass::single(ClassKind::Cleric))
        ));
        assert!(book.allows(
            "spells.cleric",
            Some(&CharacterClass::single(ClassKind::Cleric))
        ));
    }

    #[test]
    fn allows_multiclass_passes_if_either_class_matches() {
        let book = compendium_of(vec![
            test_spell("spells.mage", "abilities.mage", &[ClassKind::Mage]),
            test_spell("spells.cleric", "abilities.cleric", &[ClassKind::Cleric]),
        ]);
        let multiclass = CharacterClass {
            primary: ClassKind::Cleric,
            secondary: Some(ClassKind::Mage),
            secondary_level: 5,
            future_levels_to_secondary: false,
        };
        assert!(book.allows("spells.mage", Some(&multiclass)));
        assert!(book.allows("spells.cleric", Some(&multiclass)));
    }

    /// `ability_id` resolves to the compendium `id` for a pool/`InnateAux`
    /// activation but to the executing ability's own asset specifier for a
    /// spell wired directly into a weapon ability set (see `by_ability`'s doc
    /// comment) — `allows` must recognize a catalogued spell under either
    /// key, not just the one `SpellCompendium::get` alone covers.
    #[test]
    fn allows_matches_by_either_the_pool_id_or_the_ability_specifier() {
        let book = compendium_of(vec![test_spell(
            "spells.transmutation.plant_growth",
            "common.abilities.spells.transmutation.plant_growth",
            &[ClassKind::Druid, ClassKind::Ranger],
        )]);
        let druid = Some(CharacterClass::single(ClassKind::Druid));
        let warrior = Some(CharacterClass::single(ClassKind::Warrior));

        // Pool/InnateAux path: ability_id is the compendium id.
        assert!(book.allows("spells.transmutation.plant_growth", druid.as_ref()));
        assert!(!book.allows("spells.transmutation.plant_growth", warrior.as_ref()));

        // Direct weapon/implement-ability path: ability_id is the RON specifier.
        assert!(book.allows(
            "common.abilities.spells.transmutation.plant_growth",
            druid.as_ref()
        ));
        assert!(!book.allows(
            "common.abilities.spells.transmutation.plant_growth",
            warrior.as_ref()
        ));
    }
}
