use crate::{
    comp::{CharacterState, StateUpdate, character_state::OutputEvents},
    event::RemoteUnlockEvent,
    states::{
        behavior::{CharacterBehavior, JoinData},
        utils::*,
    },
};
use serde::{Deserialize, Serialize};
use std::time::Duration;
use vek::Vec3;

/// Separated out to condense update portions of character state
#[derive(Copy, Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct StaticData {
    /// How long the state builds up for
    pub buildup_duration: Duration,
    /// How long the state casts for. The ranged/keyless unlock effect
    /// (`RemoteUnlockEvent`) fires once, at the end of this stage.
    pub cast_duration: Duration,
    /// How long the state recovers for
    pub recover_duration: Duration,
    /// Max range the targeted sprite position can be from the caster
    pub range: f32,
    /// Adjusts turning rate during the attack
    pub ori_modifier: f32,
    /// Miscellaneous information about the ability
    pub ability_info: AbilityInfo,
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
    /// Locked at the Buildup -> Action transition (same trust model as
    /// `GroundAoe`/`Blink`'s client-selected position): the client-selected
    /// sprite position, clamped to `range` of the caster's position at that
    /// moment. A single-shot ability has nothing to re-validate mid-flight,
    /// unlike a sustained hold, so this is the only lock needed.
    pub target_pos: Option<Vec3<f32>>,
}

impl CharacterBehavior for Data {
    fn behavior(&self, data: &JoinData, output_events: &mut OutputEvents) -> StateUpdate {
        let mut update = StateUpdate::from(data);

        handle_orientation(data, &mut update, self.static_data.ori_modifier, None);
        handle_move(data, &mut update, 0.3);

        match self.stage_section {
            StageSection::Buildup => {
                if self.timer < self.static_data.buildup_duration {
                    // Build up
                    update.character = CharacterState::Knock(Data {
                        timer: tick_attack_or_default(data, self.timer, None),
                        ..*self
                    });
                } else {
                    // Lock the target: client-selected sprite pos, clamped to
                    // `range`; fall back to a point `range` ahead along the
                    // look dir if the client didn't provide one.
                    let aim = self
                        .static_data
                        .ability_info
                        .input_attr
                        .and_then(|attr| attr.select_pos)
                        .unwrap_or_else(|| {
                            data.pos.0 + *data.inputs.look_dir * self.static_data.range
                        });
                    let offset = aim - data.pos.0;
                    let target = if offset.magnitude_squared() > self.static_data.range.powi(2) {
                        data.pos.0 + offset.normalized() * self.static_data.range
                    } else {
                        aim
                    };

                    update.character = CharacterState::Knock(Data {
                        timer: Duration::default(),
                        stage_section: StageSection::Action,
                        target_pos: Some(target),
                        ..*self
                    });
                }
            },
            StageSection::Action => {
                if self.timer < self.static_data.cast_duration {
                    update.character = CharacterState::Knock(Data {
                        timer: tick_attack_or_default(data, self.timer, None),
                        ..*self
                    });
                } else {
                    // Fire the unlock effect at the locked target, once.
                    if let Some(pos) = self.target_pos {
                        output_events.emit_server(RemoteUnlockEvent {
                            entity: data.entity,
                            pos: pos.map(|e| e.floor() as i32),
                        });
                    }

                    update.character = CharacterState::Knock(Data {
                        timer: Duration::default(),
                        stage_section: StageSection::Recover,
                        ..*self
                    });
                }
            },
            StageSection::Recover => {
                if self.timer < self.static_data.recover_duration {
                    // Recovery
                    update.character = CharacterState::Knock(Data {
                        timer: tick_attack_or_default(
                            data,
                            self.timer,
                            Some(data.stats.recovery_speed_modifier),
                        ),
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

        // Allow interrupts (e.g. being staggered) same as other cast states
        handle_interrupts(data, &mut update, output_events);

        update
    }
}
