use crate::{
    comp::{CharacterState, StateUpdate, buff::BuffSource, character_state::OutputEvents},
    event::TeleportToEvent,
    states::{
        behavior::{CharacterBehavior, JoinData},
        utils::*,
    },
};
use serde::{Deserialize, Serialize};
use std::time::Duration;

/// Separated out to condense update portions of character state
#[derive(Copy, Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct StaticData {
    /// How long the state builds up for
    pub buildup_duration: Duration,
    /// How long the state recovers for
    pub recover_duration: Duration,
    /// What the max range of the teleport is
    pub max_range: f32,
    /// Miscellaneous information about the ability
    pub ability_info: AbilityInfo,
    /// Used to indicate to the frontend what ability this is for any special
    /// effects
    pub frontend_specifier: Option<FrontendSpecifier>,
    /// Where the blink lands when it must NOT be whatever the caster happens
    /// to be aiming at. `None` — the shape every previously-authored blink
    /// uses — keeps the original behaviour: targeted entity, else selected
    /// position, else 25 m forward.
    pub anchor: Option<BlinkAnchor>,
}

/// A fixed destination for a blink, resolved from the caster's own state
/// instead of from their aim.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum BlinkAnchor {
    /// Teleport to whoever is the source of the named buff currently on the
    /// caster. Content-agnostic: any buff a character grants another can be
    /// made a recall point this way (a bound talisman, a summoner's leash, a
    /// rescue ward), with the buff's own lifetime doubling as the ability's
    /// availability window.
    ///
    /// Resolves to nothing — the blink simply fizzles — if the buff is not
    /// active or was not sourced by a character.
    BuffSource(crate::comp::BuffKind),
}

#[derive(Copy, Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Data {
    /// Struct containing data that does not change over the course of the
    /// character state
    pub static_data: StaticData,
    /// Timer for each stage
    pub timer: Duration,
    /// What section the character stage is in
    pub stage_section: StageSection,
}

impl CharacterBehavior for Data {
    fn behavior(&self, data: &JoinData, output_events: &mut OutputEvents) -> StateUpdate {
        let mut update = StateUpdate::from(data);

        handle_orientation(data, &mut update, 1.0, None);

        match self.stage_section {
            StageSection::Buildup => {
                if self.timer < self.static_data.buildup_duration {
                    // Build up
                    update.character = CharacterState::Blink(Data {
                        timer: tick_attack_or_default(data, self.timer, None),
                        ..*self
                    });
                } else {
                    // Blinks to target location, defaults to 25 meters in front if no target
                    // provided. BL-05: a dimensional anchor (`Stats.disable_teleport`,
                    // from `BuffKind::Anchored`) makes the blink fizzle — buildup
                    // completes but no teleport happens.
                    if !data.stats.disable_teleport {
                        match self.static_data.anchor {
                            // An anchored blink goes where its anchor says,
                            // never where the caster happens to be aiming, and
                            // it never falls back to the aim-driven behaviour
                            // below: landing on an arbitrary target is not a
                            // degraded "recall to your anchor", it is a
                            // different ability. An anchor that resolves to
                            // nothing simply fizzles.
                            //
                            // It still emits from inside this same
                            // `disable_teleport` guard, so a dimensional
                            // anchor stops it exactly as it stops every other
                            // teleport.
                            Some(BlinkAnchor::BuffSource(kind)) => {
                                if let Some(target) = data.buffs.and_then(|buffs| {
                                    buffs
                                        .iter_kind(kind)
                                        .find_map(|(_, buff)| match buff.source {
                                            BuffSource::Character { by, .. } => Some(by),
                                            _ => None,
                                        })
                                }) {
                                    output_events.emit_server(TeleportToEvent {
                                        entity: data.entity,
                                        target,
                                        max_range: Some(self.static_data.max_range),
                                    });
                                }
                            },
                            None => {
                                if let Some(input_attr) = self.static_data.ability_info.input_attr {
                                    if let Some(target) = input_attr.target_entity {
                                        output_events.emit_server(TeleportToEvent {
                                            entity: data.entity,
                                            target,
                                            max_range: Some(self.static_data.max_range),
                                        });
                                    } else if let Some(pos) = input_attr.select_pos {
                                        update.pos.0 = pos;
                                    } else {
                                        update.pos.0 += *data.inputs.look_dir * 25.0;
                                    }
                                }
                            },
                        }
                    }
                    // Transitions to recover section of stage
                    update.character = CharacterState::Blink(Data {
                        timer: Duration::default(),
                        stage_section: StageSection::Recover,
                        ..*self
                    });
                }
            },
            StageSection::Recover => {
                if self.timer < self.static_data.recover_duration {
                    // Recovery
                    update.character = CharacterState::Blink(Data {
                        timer: tick_attack_or_default(data, self.timer, None),
                        ..*self
                    });
                } else {
                    // Done
                    end_ability(data, &mut update);
                }
            },
            _ => {
                // If it somehow ends up in an incorrect stage section
                end_ability(data, &mut update);
            },
        }

        update
    }
}

#[derive(Copy, Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub enum FrontendSpecifier {
    CultistFlame,
    FlameThrower,
}
