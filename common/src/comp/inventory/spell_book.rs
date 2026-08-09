//! Xindeler: the per-character store of *learned* spells.
//!
//! A structural clone of [`super::recipe_book::RecipeBook`], down to its
//! persistence shape: a `Vec<Item>` of [`ItemKind::SpellGroup`] items is what
//! actually lives in the database (in the `spell_book` pseudo-container), and
//! the flattened `HashSet<String>` of pool keys is derived from it on load.
//!
//! Keeping the items as the source of truth — rather than persisting bare
//! strings — means a spell page can be re-balanced by editing its RON, and
//! every character holding that page picks the change up on next login.

use crate::comp::item::{Item, ItemKind};
use hashbrown::HashSet;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct SpellBook {
    spell_groups: Vec<Item>,
    spells: HashSet<String>,
}

impl SpellBook {
    pub(super) fn len(&self) -> usize { self.spells.len() }

    pub(super) fn iter(&self) -> impl ExactSizeIterator<Item = &String> { self.spells.iter() }

    pub(super) fn iter_groups(&self) -> impl ExactSizeIterator<Item = &Item> {
        self.spell_groups.iter()
    }

    /// The flattened set of learned pool keys.
    ///
    /// Returned as a set rather than an iterator because its one consumer —
    /// `AbilityPool::for_character` — needs membership tests to deduplicate
    /// against the keys a class already grants.
    pub(super) fn spells(&self) -> &HashSet<String> { &self.spells }

    pub(super) fn reset(&mut self) {
        self.spell_groups.clear();
        self.spells.clear();
    }

    /// Pushes a group of spells to the spell book. If the group is already
    /// present the item is handed back unchanged, so the caller can put it
    /// back where it came from rather than destroying it.
    pub(super) fn push_group(&mut self, group: Item) -> Result<(), Item> {
        if self
            .spell_groups
            .iter()
            .any(|sg| sg.item_definition_id() == group.item_definition_id())
        {
            Err(group)
        } else {
            self.spell_groups.push(group);
            self.update();
            Ok(())
        }
    }

    /// Syncs the `spells` hashset with the `spell_groups` vec.
    pub(super) fn update(&mut self) {
        self.spell_groups.iter().for_each(|group| {
            if let ItemKind::SpellGroup { spells } = &*group.kind() {
                self.spells.extend(spells.iter().map(String::from))
            }
        })
    }

    pub fn spell_book_from_persistence(spell_groups: Vec<Item>) -> Self {
        let mut book = Self {
            spell_groups,
            spells: HashSet::new(),
        };
        book.update();
        book
    }

    pub fn persistence_spells_iter_with_index(&self) -> impl Iterator<Item = (usize, &Item)> {
        self.spell_groups.iter().enumerate()
    }

    pub(super) fn is_known(&self, spell_key: &str) -> bool { self.spells.contains(spell_key) }
}

#[cfg(test)]
mod tests {
    use super::SpellBook;
    use crate::comp::item::{Item, ItemKind};

    fn load_spell_items() -> Vec<Item> {
        Item::new_from_asset_glob("common.items.spell_books.*").expect("The directory should exist")
    }

    /// Every shipped spell-book item is really a `SpellGroup`, and every key it
    /// lists resolves to a real ability set — otherwise a learned spell would
    /// silently occupy an `AbilityPool` slot that casts nothing.
    #[test]
    fn shipped_spell_books_list_real_ability_keys() {
        use crate::comp::item::tool::{AbilityMap, AbilitySpec};

        let ability_map = AbilityMap::load();
        let ability_map = ability_map.read();
        let items = load_spell_items();
        assert!(
            !items.is_empty(),
            "at least one spell book asset must exist for this test to mean anything"
        );
        for item in items {
            let ItemKind::SpellGroup { spells } = &*item.kind() else {
                panic!("an item under common.items.spell_books is not a SpellGroup")
            };
            assert!(!spells.is_empty(), "a spell book with no spells is useless");
            for key in spells {
                assert!(
                    ability_map
                        .get_ability_set(&AbilitySpec::Custom(key.clone()))
                        .is_some(),
                    "spell key {key} does not resolve to an ability set"
                );
            }
        }
    }

    /// The flattened key set is the union of every group's keys, pushing the
    /// same group twice is refused rather than duplicating it, and
    /// `spell_book_from_persistence` reconstructs exactly the same set from
    /// the items alone (the shape persistence relies on).
    #[test]
    fn groups_flatten_dedup_and_survive_reconstruction() {
        let mut book = SpellBook::default();
        for item in load_spell_items() {
            book.push_group(item)
                .expect("each distinct group is accepted once");
        }
        let groups = book.iter_groups().count();
        assert!(groups > 0);

        // Every key listed by every group is now known.
        for item in load_spell_items() {
            let ItemKind::SpellGroup { spells } = &*item.kind() else {
                unreachable!("checked by the test above")
            };
            for key in spells {
                assert!(book.is_known(key), "{key} should be known after pushing");
            }
        }

        // Pushing an already-present group is refused and changes nothing.
        let duplicate = load_spell_items()
            .into_iter()
            .next()
            .expect("at least one asset");
        assert!(book.push_group(duplicate).is_err());
        assert_eq!(book.iter_groups().count(), groups);

        // The set is derived purely from the items, so rebuilding from them
        // (what load-from-DB does) yields the same knowledge.
        let rebuilt = SpellBook::spell_book_from_persistence(load_spell_items());
        assert_eq!(rebuilt.len(), book.len());
        for key in book.iter() {
            assert!(rebuilt.is_known(key));
        }
    }
}
