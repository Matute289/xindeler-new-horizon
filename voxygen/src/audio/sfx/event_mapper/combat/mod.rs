/// EventMapper::Combat watches the combat states of surrounding entities' and
/// emits sfx related to weapons and attacks/abilities
use crate::{
    AudioFrontend,
    audio::sfx::{SFX_DIST_LIMIT_SQR, SfxEvent, SfxTriggerItem, SfxTriggers},
    scene::{Camera, FigureMgr, Terrain},
};

use super::EventMapper;

use client::Client;
use common::{
    comp::{
        CharacterAbilityType, CharacterState, Inventory, Pos,
        inventory::slot::EquipSlot,
        item::{ItemKind, tool::ToolKind},
    },
    terrain::TerrainChunk,
};
use common_state::State;
use hashbrown::HashMap;
use specs::{Entity as EcsEntity, Join, LendJoin, WorldExt};
use std::time::{Duration, Instant};

#[derive(Clone)]
struct PreviousEntityState {
    event: SfxEvent,
    time: Instant,
    weapon_drawn: bool,
    /// Battle Refrain: tracked separately from `event`/`time` so a fast
    /// attack cannot starve the instrument refrain, and vice-versa.
    refrain_event: SfxEvent,
    refrain_time: Instant,
}

impl Default for PreviousEntityState {
    fn default() -> Self {
        Self {
            event: SfxEvent::Idle,
            time: Instant::now(),
            weapon_drawn: false,
            refrain_event: SfxEvent::Idle,
            refrain_time: Instant::now(),
        }
    }
}

pub struct CombatEventMapper {
    event_history: HashMap<EcsEntity, PreviousEntityState>,
}

impl EventMapper for CombatEventMapper {
    fn maintain(
        &mut self,
        audio: &mut AudioFrontend,
        state: &State,
        player_entity: specs::Entity,
        camera: &Camera,
        triggers: &SfxTriggers,
        _terrain: &Terrain<TerrainChunk>,
        _client: &Client,
        _figure_mgr: &FigureMgr,
    ) {
        let ecs = state.ecs();

        let cam_pos = camera.get_pos_with_focus();

        for (entity, pos, inventory, character) in (
            &ecs.entities(),
            &ecs.read_storage::<Pos>(),
            ecs.read_storage::<Inventory>().maybe(),
            ecs.read_storage::<CharacterState>().maybe(),
        )
            .join()
            .filter(|(_, e_pos, ..)| (e_pos.0.distance_squared(cam_pos)) < SFX_DIST_LIMIT_SQR)
        {
            if let Some(character) = character {
                let sfx_state = self.event_history.entry(entity).or_default();

                let mapped_event = inventory.map_or(SfxEvent::Idle, |inv| {
                    Self::map_event(character, sfx_state, inv)
                });

                // Check for SFX config entry for this movement
                if Self::should_emit(
                    &sfx_state.event,
                    sfx_state.time,
                    triggers.0.get_key_value(&mapped_event),
                ) {
                    let sfx_trigger_item = triggers.0.get_key_value(&mapped_event);
                    audio.emit_sfx(sfx_trigger_item, pos.0, None);
                    sfx_state.time = Instant::now();
                }

                // update state to determine the next event. We only record the time (above) if
                // it was dispatched
                sfx_state.event = mapped_event;
                sfx_state.weapon_drawn = Self::weapon_drawn(character);

                // Battle Refrain: layered on top of the event above, tracked on its
                // own independent timer so a fast attack can't starve it (or vice-versa).
                let refrain_event = inventory
                    .and_then(|inv| Self::map_refrain(character, inv))
                    .unwrap_or(SfxEvent::Idle);

                if Self::should_emit(
                    &sfx_state.refrain_event,
                    sfx_state.refrain_time,
                    triggers.0.get_key_value(&refrain_event),
                ) {
                    let sfx_trigger_item = triggers.0.get_key_value(&refrain_event);
                    audio.emit_sfx(sfx_trigger_item, pos.0, None);
                    sfx_state.refrain_time = Instant::now();
                }
                sfx_state.refrain_event = refrain_event;
            }
        }

        self.cleanup(player_entity);
    }
}

impl CombatEventMapper {
    pub fn new() -> Self {
        Self {
            event_history: HashMap::new(),
        }
    }

    /// As the player explores the world, we track the last event of the nearby
    /// entities to determine the correct SFX item to play next based on
    /// their activity. `cleanup` will remove entities from event tracking if
    /// they have not triggered an event for > n seconds. This prevents
    /// stale records from bloating the Map size.
    fn cleanup(&mut self, player: EcsEntity) {
        const TRACKING_TIMEOUT: u64 = 10;

        let now = Instant::now();
        self.event_history.retain(|entity, event| {
            now.duration_since(event.time) < Duration::from_secs(TRACKING_TIMEOUT)
                || entity.id() == player.id()
        });
    }

    /// Ensures that:
    /// 1. An sfx.ron entry exists for an SFX event
    /// 2. The sfx has not been played since it's timeout threshold has elapsed,
    ///    which prevents firing every tick
    ///
    /// Takes the previous event/time explicitly (rather than a whole
    /// `PreviousEntityState`) so the same logic can gate two independently-
    /// tracked streams (the primary event and the Battle Refrain) on the
    /// same entity without one starving the other.
    fn should_emit(
        previous_event: &SfxEvent,
        previous_time: Instant,
        sfx_trigger_item: Option<(&SfxEvent, &SfxTriggerItem)>,
    ) -> bool {
        if let Some((event, item)) = sfx_trigger_item {
            if previous_event == event {
                previous_time.elapsed().as_secs_f32() >= item.threshold
            } else {
                true
            }
        } else {
            false
        }
    }

    fn map_event(
        character_state: &CharacterState,
        previous_state: &PreviousEntityState,
        inventory: &Inventory,
    ) -> SfxEvent {
        let equip_slot = character_state
            .ability_info()
            .and_then(|ability| ability.hand)
            .map_or(EquipSlot::ActiveMainhand, |hand| hand.to_equip_slot());

        if let Some(item) = inventory.equipped(equip_slot)
            && let ItemKind::Tool(data) = &*item.kind()
        {
            if character_state.is_attack() {
                return SfxEvent::Attack(CharacterAbilityType::from(character_state), data.kind);
            } else if character_state.is_music() {
                if let Some(ability_spec) = item
                    .ability_spec()
                    .map(|ability_spec| ability_spec.into_owned())
                {
                    return SfxEvent::Music(data.kind, ability_spec);
                }
            } else if let Some(wield_event) = match (
                previous_state.weapon_drawn,
                Self::weapon_drawn(character_state),
            ) {
                (false, true) => Some(SfxEvent::Wield(data.kind)),
                (true, false) => Some(SfxEvent::Unwield(data.kind)),
                _ => None,
            } {
                return wield_event;
            }
        }
        // Check for attacking states

        SfxEvent::Idle
    }

    /// Battle Refrain: `CharacterState` is a single-slot enum, so an entity
    /// can never be simultaneously `is_attack()` and `is_music()` — casting
    /// a Bard spell through an instrument puts it in an attack state, not a
    /// music state, so `map_event` alone would never surface the
    /// instrument's sound during combat. This is a second, independent
    /// mapping over the exact same inputs: whenever the entity is mid-attack
    /// (including a spell cast) *and* its active weapon happens to be an
    /// `Instrument`, layer that instrument's `Music` event on top, purely as
    /// audio — it never changes what `map_event` itself returns.
    fn map_refrain(character_state: &CharacterState, inventory: &Inventory) -> Option<SfxEvent> {
        if !character_state.is_attack() {
            return None;
        }

        let equip_slot = character_state
            .ability_info()
            .and_then(|ability| ability.hand)
            .map_or(EquipSlot::ActiveMainhand, |hand| hand.to_equip_slot());

        let item = inventory.equipped(equip_slot)?;
        let ItemKind::Tool(data) = &*item.kind() else {
            return None;
        };
        if data.kind != ToolKind::Instrument {
            return None;
        }

        let ability_spec = item.ability_spec()?.into_owned();
        Some(SfxEvent::Music(data.kind, ability_spec))
    }

    /// This helps us determine whether we should be emitting the Wield/Unwield
    /// events. For now, consider either CharacterState::Wielding or
    /// ::Equipping to mean the weapon is drawn. This will need updating if the
    /// animations change to match the wield_duration associated with the weapon
    fn weapon_drawn(character: &CharacterState) -> bool {
        character.is_wield() || matches!(character, CharacterState::Equipping { .. })
    }
}

#[cfg(test)] mod tests;
