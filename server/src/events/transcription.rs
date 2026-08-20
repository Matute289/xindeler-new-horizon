//! Server-side resolution of [`TranscribeSpellEvent`]: copying a spell out of
//! a page listed by the caster's equipped Tome, permanently into their
//! `comp::SpellBook`.
//!
//! Validation order (all server-side, nothing here is trusted): the caster
//! holds the Mage class -> a `ToolKind::Tome` is equipped in either hand ->
//! that Tome's `spell_pages` actually lists `page` -> every spell on that
//! page passes the mastery-tier gate -> the caster can afford the gold and
//! ink cost -> the page is not already known. The Tome itself is never
//! consumed.

use crate::client::Client;
use common::{
    assets::AssetExt,
    comp::{
        AbilityPool, ActiveAbilities, Body, CharacterClass, ChatType, Content, Inventory, Pact,
        Skill, SkillSet, SpellMastery, TriggerSlots,
        ability::remap_innate_bindings,
        class::ClassKind,
        inventory::{
            item::{AbilityMap, Item, ItemDef, ItemKind, MaterialStatManifest, tool::ToolKind},
            slot::EquipSlot,
        },
        skills::MageSkill,
        spell::spell_compendium_manifest,
        transcription::{
            polyglot_cost_multiplier, spell_is_transcribable, transcription_gold_cost,
        },
    },
    event::TranscribeSpellEvent,
};
use common_net::msg::ServerGeneral;
use specs::{Entities, ReadExpect, ReadStorage, SystemData, WriteStorage, shred};
use std::sync::Arc;

use super::ServerEvent;

fn feedback(clients: &ReadStorage<'_, Client>, entity: specs::Entity, key: &str) {
    if let Some(client) = clients.get(entity) {
        client.send_fallible(ServerGeneral::server_msg(
            ChatType::CommandError,
            Content::localized(key),
        ));
    }
}

fn success(clients: &ReadStorage<'_, Client>, entity: specs::Entity) {
    if let Some(client) = clients.get(entity) {
        client.send_fallible(ServerGeneral::server_msg(
            ChatType::CommandInfo,
            Content::localized("hud-transcription-success"),
        ));
    }
}

/// The equipped Tome (if any) listing `page`, checked in both hands. Returns
/// `(any_tome_equipped, page_is_listed_by_one_of_them)` so the caller can
/// give a precise refusal reason.
fn equipped_tome_lists_page(inventory: &Inventory, page: &str) -> (bool, bool) {
    let tome_in = |slot: EquipSlot| -> Option<&Item> {
        inventory.equipped(slot).filter(
            |item| matches!(&*item.kind(), ItemKind::Tool(tool) if tool.kind == ToolKind::Tome),
        )
    };
    let tomes: Vec<&Item> = [
        tome_in(EquipSlot::ActiveMainhand),
        tome_in(EquipSlot::ActiveOffhand),
    ]
    .into_iter()
    .flatten()
    .collect();
    let any_tome = !tomes.is_empty();
    let page_listed = tomes.iter().any(|tome| {
        tome.spell_pages()
            .is_some_and(|pages| pages.iter().any(|p| p == page))
    });
    (any_tome, page_listed)
}

#[derive(SystemData)]
pub struct TranscribeSpellEventData<'a> {
    entities: Entities<'a>,
    ability_map: ReadExpect<'a, AbilityMap>,
    msm: ReadExpect<'a, MaterialStatManifest>,
    clients: ReadStorage<'a, Client>,
    character_classes: ReadStorage<'a, CharacterClass>,
    bodies: ReadStorage<'a, Body>,
    skill_sets: ReadStorage<'a, SkillSet>,
    spell_masteries: ReadStorage<'a, SpellMastery>,
    /// Read only so the pool rebuild below keeps a summoned pact blade's
    /// attack keys; nothing here ever mutates a `Pact`.
    pacts: ReadStorage<'a, Pact>,
    inventories: WriteStorage<'a, Inventory>,
    ability_pools: WriteStorage<'a, AbilityPool>,
    active_abilities: WriteStorage<'a, ActiveAbilities>,
    trigger_slots: WriteStorage<'a, TriggerSlots>,
}

impl ServerEvent for TranscribeSpellEvent {
    type SystemData<'a> = TranscribeSpellEventData<'a>;

    fn handle(events: impl ExactSizeIterator<Item = Self>, mut data: Self::SystemData<'_>) {
        let compendium = spell_compendium_manifest();
        let coin_def = Arc::<ItemDef>::load_cloned("common.items.utility.coins")
            .expect("common.items.utility.coins must exist");
        let ink_def = Arc::<ItemDef>::load_cloned("common.items.crafting_ing.arcane_ink")
            .expect("common.items.crafting_ing.arcane_ink must exist");

        for ev in events {
            if !data.entities.is_alive(ev.entity) {
                continue;
            }

            // Gate 1: class.
            let is_mage = data
                .character_classes
                .get(ev.entity)
                .is_some_and(|class| class.has(ClassKind::Mage));
            if !is_mage {
                feedback(&data.clients, ev.entity, "hud-transcription-wrong-class");
                continue;
            }

            let Some(inventory) = data.inventories.get(ev.entity) else {
                continue;
            };

            // Gate 2: a Tome is equipped and lists `page`.
            let (any_tome, page_listed) = equipped_tome_lists_page(inventory, &ev.page);
            if !any_tome {
                feedback(&data.clients, ev.entity, "hud-transcription-no-tome");
                continue;
            }
            if !page_listed {
                feedback(
                    &data.clients,
                    ev.entity,
                    "hud-transcription-page-not-in-tome",
                );
                continue;
            }

            // Load the page itself -- a fresh, un-owned `Item`, not taken
            // from any inventory. The Tome that lists it is never touched.
            let Ok(page_item) = Item::new_from_asset(&ev.page) else {
                continue;
            };
            let ItemKind::SpellGroup { spells } = &*page_item.kind() else {
                continue;
            };

            // Gate 3: mastery tier, per spell on the page.
            let mastery = data
                .spell_masteries
                .get(ev.entity)
                .copied()
                .unwrap_or_default();
            let all_transcribable = spells
                .iter()
                .all(|id| spell_is_transcribable(id, &mastery, &compendium));
            if !all_transcribable {
                feedback(
                    &data.clients,
                    ev.entity,
                    "hud-transcription-mastery-too-low",
                );
                continue;
            }

            // Gate 4: affordability (checked, not yet spent).
            let polyglot_rank = data
                .skill_sets
                .get(ev.entity)
                .map(|ss| {
                    ss.skill_level(Skill::Mage(MageSkill::Polyglot))
                        .unwrap_or(0)
                })
                .unwrap_or(0);
            let discount = polyglot_cost_multiplier(polyglot_rank);
            let gold_cost = (spells
                .iter()
                .map(|id| transcription_gold_cost(compendium.get(id).map(|s| s.level)))
                .sum::<u32>() as f32
                * discount)
                .round() as u32;
            const INK_COST: u32 = 1;

            if inventory.item_count(&coin_def) < u64::from(gold_cost) {
                feedback(
                    &data.clients,
                    ev.entity,
                    "hud-transcription-insufficient-gold",
                );
                continue;
            }
            if inventory.item_count(&ink_def) < u64::from(INK_COST) {
                feedback(
                    &data.clients,
                    ev.entity,
                    "hud-transcription-insufficient-ink",
                );
                continue;
            }

            // Perform the copy. `push_spell_group` refuses (and hands the
            // item back) if this exact page is already known -- no cost is
            // spent in that case.
            let old_pool = data
                .ability_pools
                .get(ev.entity)
                .cloned()
                .unwrap_or_default();
            let learned = {
                let Some(mut inventory) = data.inventories.get_mut(ev.entity) else {
                    continue;
                };
                match inventory.push_spell_group(page_item) {
                    Ok(()) => inventory.learned_spells().clone(),
                    Err(_) => {
                        feedback(&data.clients, ev.entity, "hud-transcription-already-known");
                        continue;
                    },
                }
            };

            // Cost is spent only now that the copy actually happened.
            if gold_cost > 0
                && let Some(mut inventory) = data.inventories.get_mut(ev.entity)
            {
                let _ = inventory.remove_item_amount(
                    &coin_def,
                    gold_cost,
                    &data.ability_map,
                    &data.msm,
                );
            }
            if let Some(mut inventory) = data.inventories.get_mut(ev.entity) {
                let _ =
                    inventory.remove_item_amount(&ink_def, INK_COST, &data.ability_map, &data.msm);
            }

            // The pool is derived state; rebuild it now rather than waiting
            // for a relog. Mirrors `/learn_spells` (`server/src/cmd.rs`),
            // including the two `remap_innate_bindings` call sites -- the
            // hotbar and reactive trigger slots both hold raw pool indices.
            if let (Some(body), Some(character_class)) = (
                data.bodies.get(ev.entity).copied(),
                data.character_classes.get(ev.entity).copied(),
            ) {
                // `for_character_with_pact`, not `for_character`: a Warlock
                // with the blade out must not lose its three attack keys (and
                // the hotbar slots bound to them) just because they
                // transcribed a spell. `.with_talisman_bond(..)` chained the
                // same way: `for_character_with_pact` only knows about ITS
                // OWN entity's `Pact` (the blade grant), never about a live
                // talisman bond, which is keyed on the BEARER's pool and can
                // belong to an entity with no `Pact` of its own at all --
                // without re-asserting it here from `old_pool`, a bonded
                // bearer who transcribes a spell would silently lose their
                // recall key and its hotbar slot.
                let new_pool = AbilityPool::for_character_with_pact(
                    &body,
                    &character_class,
                    &learned,
                    data.pacts.get(ev.entity),
                )
                .with_talisman_bond(old_pool.has_talisman_bond());
                if let Some(mut active) = data.active_abilities.get_mut(ev.entity) {
                    remap_innate_bindings(&mut active, &old_pool, &new_pool);
                }
                if let Some(mut slots) = data.trigger_slots.get_mut(ev.entity) {
                    slots.remap_innate_bindings(&old_pool, &new_pool);
                }
                let _ = data.ability_pools.insert(ev.entity, new_pool);
            }

            success(&data.clients, ev.entity);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use common::{comp::inventory::slot::EquipSlot, resources::Time};

    fn inventory_with_tome_in(slot: EquipSlot, asset: &str) -> Inventory {
        let mut inv = Inventory::with_empty();
        inv.replace_loadout_item(slot, Some(Item::new_from_asset_expect(asset)), Time(0.0));
        inv
    }

    #[test]
    fn no_tome_equipped_reports_no_tome() {
        let inv = Inventory::with_empty();
        let (any_tome, page_listed) =
            equipped_tome_lists_page(&inv, "common.items.testing.test_page_divine_l1");
        assert!(!any_tome);
        assert!(!page_listed);
    }

    #[test]
    fn a_tome_with_no_matching_page_reports_tome_but_not_listed() {
        // apprentice_tome.ron declares no spell_pages at all.
        let inv = inventory_with_tome_in(
            EquipSlot::ActiveMainhand,
            "common.items.weapons.tome.apprentice_tome",
        );
        let (any_tome, page_listed) =
            equipped_tome_lists_page(&inv, "common.items.testing.test_page_divine_l1");
        assert!(any_tome);
        assert!(!page_listed);
    }

    #[test]
    fn a_tome_listing_the_page_is_found_in_either_hand() {
        for slot in [EquipSlot::ActiveMainhand, EquipSlot::ActiveOffhand] {
            let inv =
                inventory_with_tome_in(slot, "common.items.testing.test_tome_of_transcription");
            let (any_tome, page_listed) =
                equipped_tome_lists_page(&inv, "common.items.testing.test_page_divine_l1");
            assert!(any_tome, "{slot:?}");
            assert!(page_listed, "{slot:?}");
        }
    }

    #[test]
    fn a_page_absent_from_an_otherwise_matching_tome_is_not_listed() {
        let inv = inventory_with_tome_in(
            EquipSlot::ActiveMainhand,
            "common.items.testing.test_tome_of_transcription",
        );
        let (any_tome, page_listed) =
            equipped_tome_lists_page(&inv, "common.items.testing.not_a_real_page");
        assert!(any_tome);
        assert!(!page_listed);
    }
}
