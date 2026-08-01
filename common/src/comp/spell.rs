//! Spell compendium metadata (magic-system-v2 spec §3). A metadata layer over
//! the ability pipeline: every `SpellDef` points at a `CharacterAbility` RON
//! that actually executes, and combat reads that ability directly rather than
//! this file. What DOES read this file is two-fold: presentational (spellbook
//! UI, tooltips) AND load-bearing cast-time authorization — every ability
//! activation, of any kind, is checked against `SpellCompendium::allows`
//! (`states::utils::handle_ability`), so an entry catalogued here can refuse
//! a class from casting an ability regardless of what equipped it. See
//! `SpellDef::pool_eligible` for the one case where a catalogued entry exists
//! ONLY for that authorization check and must not also grant the ability
//! through the innate spell pool.
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

/// Default for [`SpellDef::pool_eligible`] on any entry that omits the field
/// -- every pre-existing compendium entry is a real pool spell, so silently
/// defaulting to `false` would strip them all from every class's ability pool
/// the moment this field shipped. Only content that explicitly opts out (see
/// the field's own doc comment) sets `pool_eligible: false`.
fn default_pool_eligible() -> bool { true }

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
    /// Whether this entry may be granted as an innate, weaponless
    /// `AbilityPool` slot (see [`SpellCompendium::spells_for_class`]).
    /// Defaults to `true` (via [`default_pool_eligible`]) for every entry
    /// that doesn't set it, which is every spell catalogued before this
    /// field existed.
    ///
    /// `false` is for cataloguing an ability SOLELY to run it through the
    /// per-spell `classes` filter in [`SpellCompendium::allows`] (checked at
    /// cast time in `states::utils::handle_ability`, independent of the
    /// activation pathway -- see `by_ability`'s doc comment) without also
    /// granting it as a free weaponless spell. A weapon ability set (e.g. a
    /// `Tool(ToolKind)` entry in `ability_set_manifest.ron`) can wire an
    /// ability directly into its `primary`/`secondary`/`abilities` slots, so
    /// it is already castable by any class holding that tool -- cataloguing
    /// such an ability with `pool_eligible: true` would additionally make
    /// every listed class able to cast it with NO weapon equipped at all,
    /// because `AbilityPool::for_character` embeds `spells_for_class`
    /// unconditionally. That is a new capability the class-gate alone should
    /// never grant. `false` keeps the class check while refusing that pool
    /// grant.
    #[serde(default = "default_pool_eligible")]
    pub pool_eligible: bool,
}

impl SpellDef {
    /// The `AbilityPool` / `AbilityMap` key for this spell. The compendium
    /// `id` is already in `"spells.<school>.<name>"` shape, so it doubles as
    /// the `AbilitySpec::Custom` key, the frontend ability id, and the i18n
    /// key stem -- exactly like the `class.*` and `innate.*` keys.
    ///
    /// Still well-defined for a `pool_eligible: false` entry (it is what
    /// `AbilityMap::load` injects as its `Custom(id)` key); it is simply
    /// never referenced by any character's actual `AbilityPool.abilities`,
    /// since `spells_for_class` excludes those entries.
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
    ///
    /// Excludes `pool_eligible: false` entries -- content catalogued only to
    /// run the [`Self::allows`] class check on a weapon-bound ability, never
    /// meant to be grantable as an innate weaponless spell. See that field's
    /// doc comment. This is the sole choke point `AbilityPool::for_character`
    /// reads spells through, so excluding an entry here is what keeps it out
    /// of every character's pool.
    pub fn spells_for_class(&self, class: ClassKind) -> Vec<&SpellDef> {
        let mut out: Vec<&SpellDef> = self
            .spells
            .iter()
            .filter(|s| s.classes.contains(&class) && s.pool_eligible)
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

    /// The cast-time per-spell class filter, applied in
    /// `states::utils::handle_ability`. A spell's own `classes` list is the
    /// ONLY class-side restriction on casting: `SpellDef::source` records
    /// where the magic comes from and never narrows who may cast it, so no
    /// class-to-source rule exists (or may be reintroduced) here.
    ///
    /// `ability_id` is whatever `SpecifiedAbility::ability_id` resolved for
    /// the activation — checked against both `by_id` (pool/`InnateAux`
    /// spells) and `by_ability` (a spell wired directly into a
    /// weapon/implement ability set), so this catches a catalogued spell
    /// regardless of which path delivered it.
    ///
    /// Three cases pass unconditionally, each backing a specific shipped
    /// design:
    /// - `ability_id` matches no compendium entry by either key — most
    ///   abilities aren't catalogued at all; absence is not denial.
    /// - `character_class` is `None` — an entity with no `CharacterClass`
    ///   (every NPC, summon and boss) keeps casting; permissive by absence.
    /// - the caster holds `ClassKind::Adventurer` — the legacy pre-class value;
    ///   most catalogued entries only list `Mage`, so gating Adventurer would
    ///   strip every legacy character's kit.
    ///
    /// Otherwise passes when any class the caster holds
    /// (`CharacterClass::classes`) appears in the entry's `classes` list —
    /// a multiclass character needs only one of its two classes to match.
    ///
    /// Deliberately has no `Ability::InnateAux` exemption: every catalogued
    /// spell for a held class reaches `handle_ability` as `InnateAux`
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
/// `by_ability` indices (mirrors `class::class_proficiencies_manifest`'s own
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

    /// Design invariant: **a spell's `classes` list is the only thing that
    /// decides who may cast it**. A spell's `source` is provenance/flavor,
    /// authored per spell, but classes are NOT bound to sources — so no
    /// source-derived rule may ever refuse a class the compendium itself
    /// lists on that spell.
    ///
    /// Guards the link this invariant actually runs through: `allows` must
    /// never refuse a listed class under EITHER keying. The `by_ability` half
    /// is the one that can genuinely break — two entries sharing one
    /// executing ability RON collapse to "first entry wins" (see
    /// `by_ability`'s doc comment), which would silently answer with the
    /// wrong entry's `classes` list. A gate reintroduced elsewhere in the
    /// activation chain is out of this test's reach.
    #[test]
    fn every_class_the_compendium_lists_can_cast_that_spell() {
        let book = SpellCompendium::load_expect_cloned();
        let mut refused: Vec<String> = Vec::new();

        for spell in book.iter() {
            for &class in &spell.classes {
                let character_class = CharacterClass::single(class);
                // Both keyings `allows` is reachable under: the pool/InnateAux
                // path (compendium id) and the direct weapon-ability path
                // (the executing ability's own specifier).
                if !book.allows(&spell.id, Some(&character_class))
                    || !book.allows(&spell.ability, Some(&character_class))
                {
                    refused.push(format!(
                        "{} ({:?}) refused for {class:?}",
                        spell.id, spell.source
                    ));
                }
            }
        }

        assert!(
            refused.is_empty(),
            "a class the compendium lists on a spell cannot cast it:\n{}",
            refused.join("\n")
        );
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
            pool_eligible: true,
        }
    }

    /// Like [`test_spell`] but `pool_eligible: false` -- a weapon-bound
    /// ability catalogued only for its cast-time class check, never meant to
    /// be grantable as an innate weaponless spell.
    fn test_weapon_bound_spell(id: &str, ability: &str, classes: &[ClassKind]) -> SpellDef {
        SpellDef {
            pool_eligible: false,
            ..test_spell(id, ability, classes)
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

    /// `pool_eligible: false` must not weaken `allows` at all -- the class
    /// check is orthogonal to whether the entry may also be granted as a
    /// weaponless pool spell.
    #[test]
    fn pool_ineligible_spell_still_gates_by_ability_id() {
        let book = compendium_of(vec![test_weapon_bound_spell(
            "legacy.staff.firebomb",
            "common.abilities.staff.firebomb",
            &[ClassKind::Mage],
        )]);
        let mage = Some(CharacterClass::single(ClassKind::Mage));
        let warrior = Some(CharacterClass::single(ClassKind::Warrior));

        assert!(book.allows("common.abilities.staff.firebomb", mage.as_ref()));
        assert!(!book.allows("common.abilities.staff.firebomb", warrior.as_ref()));
    }

    /// The actual bug this field exists to avoid: a `pool_eligible: false`
    /// entry must never appear in `spells_for_class`, or a class merely
    /// catalogued on a weapon-bound ability would gain it as a free
    /// weaponless spell via `AbilityPool::for_character`.
    #[test]
    fn pool_ineligible_spell_is_excluded_from_spells_for_class() {
        let book = compendium_of(vec![
            test_spell("spells.mage.cinder", "abilities.cinder", &[ClassKind::Mage]),
            test_weapon_bound_spell(
                "legacy.staff.firebomb",
                "common.abilities.staff.firebomb",
                &[ClassKind::Mage],
            ),
        ]);
        let mage_spells = book.spells_for_class(ClassKind::Mage);
        assert_eq!(mage_spells.len(), 1);
        assert_eq!(mage_spells[0].id, "spells.mage.cinder");
    }

    /// Real-content guard: every legacy Staff/Sceptre ability catalogued
    /// under the `legacy.` namespace is `pool_eligible: false` -- if a future
    /// edit to `compendium.ron` flips this by accident, every class listed
    /// on it silently gains a free weaponless cast of a full attack spell.
    #[test]
    fn legacy_weapon_bound_entries_are_pool_ineligible() {
        let book = SpellCompendium::load_expect_cloned();
        let legacy: Vec<&SpellDef> = book
            .iter()
            .filter(|s| s.id.starts_with("legacy."))
            .collect();
        assert!(
            !legacy.is_empty(),
            "expected the legacy Staff/Sceptre kit to be catalogued"
        );
        for spell in &legacy {
            assert!(
                !spell.pool_eligible,
                "{} must be pool_eligible: false",
                spell.id
            );
            for &class in &spell.classes {
                assert!(
                    !book
                        .spells_for_class(class)
                        .iter()
                        .any(|s| s.id == spell.id),
                    "{} leaked into {class:?}'s ability pool",
                    spell.id
                );
            }
        }
    }

    /// Real-content guard for the actual gap this catalogue closes: every
    /// legacy Staff ability is castable by the Staff-proficient caster
    /// classes and refused for a class that was never meant to hold a Staff,
    /// and likewise for Sceptre -- checked through `allows` exactly as
    /// `states::utils::handle_ability` checks it, so this is independent of
    /// which item (standard or modular) resolved the ability.
    #[test]
    fn legacy_staff_and_sceptre_abilities_are_class_gated() {
        let book = SpellCompendium::load_expect_cloned();
        let staff_classes = [
            ClassKind::Mage,
            ClassKind::Sorcerer,
            ClassKind::Warlock,
            ClassKind::Druid,
        ];
        let sceptre_classes = [ClassKind::Cleric, ClassKind::Druid];
        let outsider = CharacterClass::single(ClassKind::Warrior);

        for ability in [
            "common.abilities.staff.firebomb",
            "common.abilities.staff.flamethrower",
            "common.abilities.staff.fireshockwave",
            "common.abilities.staff.napalm_strike",
            "common.abilities.staff.flame_cloak",
            "common.abilities.staff.fire_dash",
            "common.abilities.staff.fire_breath",
            "common.abilities.staff.pyroclasm",
        ] {
            for &class in &staff_classes {
                assert!(
                    book.allows(ability, Some(&CharacterClass::single(class))),
                    "{class:?} should be able to cast {ability}"
                );
            }
            assert!(
                !book.allows(ability, Some(&outsider)),
                "Warrior must not be able to cast {ability}"
            );
        }

        for ability in [
            "common.abilities.sceptre.lifestealbeam",
            "common.abilities.sceptre.healingaura",
            "common.abilities.sceptre.wardingaura",
        ] {
            for &class in &sceptre_classes {
                assert!(
                    book.allows(ability, Some(&CharacterClass::single(class))),
                    "{class:?} should be able to cast {ability}"
                );
            }
            assert!(
                !book.allows(ability, Some(&outsider)),
                "Warrior must not be able to cast {ability}"
            );
        }
    }

    /// The Staff/Sceptre class roster is authored in three places that
    /// nothing else derives from one another: `class_proficiencies.ron`
    /// (who may equip at all), the standard `ItemDef.requirements.classes`
    /// on each shipped item (the equip gate), and the `classes:` list on
    /// these `legacy.*` compendium entries (the cast-time gate, which also
    /// covers modular/crafted items the equip gate cannot reach). Guards
    /// against the equip gate and the cast gate drifting apart -- if a
    /// future edit changes one without the other, a standard item and a
    /// modular item of the same kind would enforce different class lists.
    #[test]
    fn legacy_kit_classes_match_the_standard_item_equip_gate() {
        use crate::comp::inventory::item::Item;

        let book = SpellCompendium::load_expect_cloned();

        let staff_items = [
            "common.items.weapons.staff.cultist_staff",
            "common.items.weapons.staff.laevateinn",
            "common.items.weapons.staff.staff_1",
            "common.items.weapons.staff.starter_staff",
        ];
        let sceptre_items = [
            "common.items.weapons.sceptre.amethyst",
            "common.items.weapons.sceptre.belzeshrub",
            "common.items.weapons.sceptre.caduceus",
            "common.items.weapons.sceptre.root_evil",
            "common.items.weapons.sceptre.sceptre_velorite_0",
            "common.items.weapons.sceptre.starter_sceptre",
        ];

        let legacy_staff_classes: Vec<&SpellDef> = book
            .iter()
            .filter(|s| s.id.starts_with("legacy.staff."))
            .collect();
        let legacy_sceptre_classes: Vec<&SpellDef> = book
            .iter()
            .filter(|s| s.id.starts_with("legacy.sceptre."))
            .collect();
        assert!(!legacy_staff_classes.is_empty());
        assert!(!legacy_sceptre_classes.is_empty());

        for (item_ids, legacy_spells, kind) in [
            (&staff_items[..], &legacy_staff_classes, "staff"),
            (&sceptre_items[..], &legacy_sceptre_classes, "sceptre"),
        ] {
            for item_id in item_ids {
                let item = Item::new_from_asset_expect(item_id);
                let equip_gate = item
                    .requirements()
                    .and_then(|r| r.classes.as_ref())
                    .unwrap_or_else(|| panic!("{item_id} has no requirements.classes equip gate"));

                for spell in legacy_spells {
                    let mut equip_gate_sorted = equip_gate.clone();
                    equip_gate_sorted.sort_by_key(|c| format!("{c:?}"));
                    let mut spell_classes_sorted = spell.classes.clone();
                    spell_classes_sorted.sort_by_key(|c| format!("{c:?}"));
                    assert_eq!(
                        spell_classes_sorted, equip_gate_sorted,
                        "legacy {kind} ability {} classes {:?} do not match {item_id}'s equip \
                         gate {:?}",
                        spell.id, spell.classes, equip_gate,
                    );
                }
            }
        }
    }
}
