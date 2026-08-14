use common::comp::{
    self,
    inventory::item::{Item, item_key::ItemKey},
};
use serde::{Deserialize, Serialize};

use super::HudInfo;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Slot {
    #[default]
    One = 0,
    Two = 1,
    Three = 2,
    Four = 3,
    Five = 4,
    Six = 5,
    Seven = 6,
    Eight = 7,
    Nine = 8,
    Ten = 9,
    /// Bound here, this ability replaces the equipped weapon's primary
    /// (left-click) combo -- unbound, left-click behaves exactly as before.
    MouseLeft = 10,
    /// Same as `MouseLeft`, but for the weapon's secondary (right-click)
    /// combo.
    MouseRight = 11,
}

pub const SLOT_COUNT: usize = 12;

#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub enum SlotContents {
    Inventory(u64, ItemKey),
    Ability(usize),
}

#[derive(Clone, Default)]
pub struct State {
    pub slots: [Option<SlotContents>; SLOT_COUNT],
    inputs: [bool; SLOT_COUNT],
    pub currently_selected_slot: Slot,
}

impl State {
    pub fn new(slots: [Option<SlotContents>; SLOT_COUNT]) -> Self {
        Self {
            slots,
            inputs: [false; SLOT_COUNT],
            currently_selected_slot: Slot::default(),
        }
    }

    /// Returns true if the button was just pressed
    pub fn process_input(&mut self, slot: Slot, state: bool) -> bool {
        let slot = slot as usize;
        let just_pressed = !self.inputs[slot] && state;
        self.inputs[slot] = state;
        just_pressed
    }

    pub fn get(&self, slot: Slot) -> Option<SlotContents> { self.slots[slot as usize].clone() }

    pub fn swap(&mut self, a: Slot, b: Slot) { self.slots.swap(a as usize, b as usize); }

    pub fn clear_slot(&mut self, slot: Slot) { self.slots[slot as usize] = None; }

    pub fn add_inventory_link(&mut self, slot: Slot, item: &Item) {
        self.slots[slot as usize] = Some(SlotContents::Inventory(
            item.item_hash(),
            ItemKey::from(item),
        ));
    }

    // TODO: remove pending UI
    // Adds ability slots if missing and should be present
    // Removes ability slots if not there and shouldn't be present
    pub fn maintain_abilities(&mut self, client: &client::Client, info: &HudInfo) {
        use specs::WorldExt;
        if let Some(active_abilities) = client
            .state()
            .ecs()
            .read_storage::<comp::ActiveAbilities>()
            .get(info.viewpoint_entity)
        {
            use common::comp::ability::AuxiliaryAbility;
            for ((i, ability), hotbar_slot) in active_abilities
                .auxiliary_set(
                    client.inventories().get(info.viewpoint_entity),
                    client
                        .state()
                        .read_storage::<comp::SkillSet>()
                        .get(info.viewpoint_entity),
                )
                .iter()
                .enumerate()
                .zip(self.slots.iter_mut())
            {
                if matches!(ability, AuxiliaryAbility::Empty) {
                    if matches!(hotbar_slot, Some(SlotContents::Ability(_))) {
                        // If ability is empty but hotbar shows an ability, clear it
                        *hotbar_slot = None;
                    }
                } else {
                    // If an ability is not empty show it on the hotbar
                    *hotbar_slot = Some(SlotContents::Ability(i));
                }
            }
        } else {
            self.slots
                .iter_mut()
                .filter(|slot| matches!(slot, Some(SlotContents::Ability(_))))
                .for_each(|slot| *slot = None)
        }
    }
}

impl Slot {
    const SLOTS: [Slot; 10] = [
        Slot::One,
        Slot::Two,
        Slot::Three,
        Slot::Four,
        Slot::Five,
        Slot::Six,
        Slot::Seven,
        Slot::Eight,
        Slot::Nine,
        Slot::Ten,
    ];

    pub fn next_slot(&mut self) {
        let current_slot = *self as usize;
        let next_slot = (current_slot + 1) % 10;
        *self = Self::SLOTS[next_slot];
    }

    pub fn previous_slot(&mut self) {
        let current_slot = *self as usize;
        let previous_slot = (current_slot + 10 - 1) % 10;
        *self = Self::SLOTS[previous_slot];
    }
}

impl State {
    /// The ability bound to left-click (if any), overriding the equipped
    /// weapon's primary combo while it's set.
    pub fn mouse_left_override(&self) -> Option<SlotContents> { self.get(Slot::MouseLeft) }

    /// The ability bound to right-click (if any), overriding the equipped
    /// weapon's secondary combo while it's set.
    pub fn mouse_right_override(&self) -> Option<SlotContents> { self.get(Slot::MouseRight) }
}
