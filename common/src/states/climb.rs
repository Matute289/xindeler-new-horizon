use crate::{
    comp::{
        CharacterState, InputKind, Ori, StateUpdate,
        character_state::OutputEvents,
        skills::{ClimbSkill::*, SKILL_MODIFIERS, Skill},
    },
    consts::GRAVITY,
    states::{
        behavior::{CharacterBehavior, JoinData},
        idle,
        utils::*,
    },
    util::Dir,
};
use serde::{Deserialize, Serialize};
use vek::*;

use super::wielding;

/// Separated out to condense update portions of character state
#[derive(Copy, Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct StaticData {
    pub energy_cost: f32,
    pub movement_speed: f32,
}

#[derive(Copy, Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Data {
    /// Struct containing data that does not change over the course of the
    /// character state
    pub static_data: StaticData,
    pub was_wielded: bool,
}

impl Data {
    pub fn create_adjusted_by_skills(join_data: &JoinData) -> Self {
        let modifiers = SKILL_MODIFIERS.general_tree.climb;
        let mut data = Data::default();
        if let Ok(level) = join_data.skill_set.skill_level(Skill::Climb(Cost)) {
            data.static_data.energy_cost *= modifiers.energy_cost.powi(level.into());
        }
        if let Ok(level) = join_data.skill_set.skill_level(Skill::Climb(Speed)) {
            data.static_data.movement_speed *= modifiers.speed.powi(level.into());
        }
        data
    }

    pub fn with_wielded(self, was_wielded: bool) -> Self {
        Self {
            was_wielded,
            ..self
        }
    }

    fn update_state_on_leaving(&self, update: &mut StateUpdate) {
        if self.was_wielded {
            update.character = CharacterState::Wielding(wielding::Data { is_sneaking: false });
        } else {
            update.character = CharacterState::Idle(idle::Data::default());
        }
    }
}

/// Energy drained per second of sustained upward climbing, before skills.
pub const CLIMB_ENERGY_COST: f32 = 15.0;

/// The energy below which starting a climb is not worth it.
///
/// `handle_climb` itself only refuses below `1.0` — enough to enter the state
/// and be thrown off it a few hundredths of a second later, which is fine for a
/// player who chose to try and useless for an NPC deciding whether to commit.
/// Expressed against [`CLIMB_ENERGY_COST`] rather than as a bare number so the
/// two cannot drift apart: this is roughly two thirds of a second of climbing.
pub const MIN_CLIMB_COMMIT_ENERGY: f32 = CLIMB_ENERGY_COST * 0.67;

impl Default for Data {
    fn default() -> Self {
        Data {
            static_data: StaticData {
                energy_cost: CLIMB_ENERGY_COST,
                movement_speed: 5.0,
            },
            was_wielded: false,
        }
    }
}

impl CharacterBehavior for Data {
    fn behavior(&self, data: &JoinData, _output_events: &mut OutputEvents) -> StateUpdate {
        let mut update = StateUpdate::from(data);

        let Some(wall_dir) = data.physics.on_wall else {
            self.update_state_on_leaving(&mut update);
            return update;
        };

        // Exit climb if ground below
        if data.physics.on_ground.is_some() {
            self.update_state_on_leaving(&mut update);
            return update;
        }

        // Set orientation based on wall direction
        if let Some(ori_dir) = Dir::from_unnormalized(wall_dir.with_z(0.0)) {
            // Smooth orientation
            update.ori = update.ori.slerped_towards(
                Ori::from(ori_dir),
                if data.physics.on_ground.is_some() {
                    9.0
                } else {
                    2.0
                } * data.dt.0,
            );
        };
        // Map movement direction onto wall
        let upwards_vel = data.inputs.move_dir.dot(wall_dir.xy());
        let crossed = wall_dir.cross(Vec3::unit_z());
        let lateral_vel = crossed * data.inputs.move_dir.dot(crossed.xy());

        let energy_use = lateral_vel
            .with_z(upwards_vel.max(0.0) * self.static_data.energy_cost)
            .magnitude()
            .max(1.0);

        // Update energy and exit climbing state if not enough
        if update
            .energy
            .try_change_by(-energy_use * data.dt.0)
            .is_err()
        {
            self.update_state_on_leaving(&mut update);
        }

        // By default idle on wall
        update.vel.0.z += data.dt.0 * GRAVITY;

        update.vel.0 += data.dt.0
            * (lateral_vel.with_z(upwards_vel) + wall_dir)
            * self.static_data.movement_speed.powi(2)
            * data.scale.map_or(1.0, |s| s.0);

        update
    }

    fn stand(&self, data: &JoinData, _output_events: &mut OutputEvents) -> StateUpdate {
        let mut update = StateUpdate::from(data);
        self.update_state_on_leaving(&mut update);
        update
    }

    fn on_input(
        &self,
        data: &JoinData,
        input: InputKind,
        output_events: &mut OutputEvents,
    ) -> StateUpdate {
        let mut update = StateUpdate::from(data);
        if matches!(input, InputKind::WallJump) {
            handle_walljump(data, output_events, &mut update, self.was_wielded);
        }
        update
    }
}
