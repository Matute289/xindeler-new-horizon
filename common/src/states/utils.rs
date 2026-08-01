use crate::{
    astar::Astar,
    comp::{
        Alignment, Body, CharacterState, Density, HealthChange, InputAttr, InputKind, Inventory,
        InventoryAction, Melee, Ori, Pos, Scale, StateUpdate,
        ability::{
            AbilityInitEvent, AbilityMeta, AbilityRequirements, Capability, CharacterAbility,
            SpecifiedAbility, Stance,
        },
        arthropod, biped_large, biped_small, bird_medium,
        buff::{Buff, BuffCategory, BuffChange, BuffData, BuffSource, DestInfo},
        character_state::OutputEvents,
        class::{CharacterClass, ClassKind},
        controller::InventoryManip,
        crustacean, golem,
        inventory::slot::{ArmorSlot, EquipSlot, Slot},
        item::{Hands, ItemKind, ToolKind, WeaponRole, armor::Friction, tool},
        object, quadruped_low, quadruped_medium, quadruped_small, ship,
        skills::{SKILL_MODIFIERS, Skill, SwimSkill},
        spell::spell_compendium_manifest,
        theropod,
    },
    consts::{FRIC_GROUND, GRAVITY, MAX_MOUNT_RANGE, MAX_PICKUP_RANGE},
    event::{
        BuffEvent, ChangeStanceEvent, ComboChangeEvent, HealthChangeEvent, InventoryManipEvent,
        LocalEvent, SetAbilityCooldownEvent,
    },
    mounting::Volume,
    outcome::Outcome,
    states::{behavior::JoinData, utils::CharacterState::Idle, *},
    terrain::{Block, TerrainGrid, UnlockKind},
    uid::Uid,
    util::Dir,
    vol::ReadVol,
};
use core::hash::BuildHasherDefault;
use fxhash::FxHasher64;
use itertools::Either;
use rand::RngExt;
use serde::{Deserialize, Serialize};
use std::{
    f32::consts::PI,
    num::NonZeroU32,
    ops::{Add, Div, Mul},
    time::Duration,
};
use strum::Display;
use tracing::warn;
use vek::*;

pub const MOVEMENT_THRESHOLD_VEL: f32 = 3.0;

impl Body {
    pub fn base_accel(&self) -> f32 {
        match self {
            // Note: Entities have been slowed down relative to humanoid speeds, but it may be worth
            // reverting/increasing speed once we've established slower AI.
            Body::Humanoid(_) => 100.0,
            Body::QuadrupedSmall(body) => match body.species {
                quadruped_small::Species::Turtle => 30.0,
                quadruped_small::Species::Axolotl => 70.0,
                quadruped_small::Species::Pig => 70.0,
                quadruped_small::Species::Sheep => 70.0,
                quadruped_small::Species::Truffler => 70.0,
                quadruped_small::Species::Fungome => 70.0,
                quadruped_small::Species::Goat => 80.0,
                quadruped_small::Species::Raccoon => 100.0,
                quadruped_small::Species::Frog => 150.0,
                quadruped_small::Species::Porcupine => 100.0,
                quadruped_small::Species::Beaver => 100.0,
                quadruped_small::Species::Rabbit => 110.0,
                quadruped_small::Species::Cat => 150.0,
                quadruped_small::Species::Quokka => 100.0,
                quadruped_small::Species::MossySnail => 20.0,
                _ => 125.0,
            },
            Body::QuadrupedMedium(quadruped_medium) => match quadruped_medium.species {
                quadruped_medium::Species::Grolgar => 100.0,
                quadruped_medium::Species::Saber => 110.0,
                quadruped_medium::Species::Tiger => 110.0,
                quadruped_medium::Species::Tuskram => 85.0,
                quadruped_medium::Species::Lion => 105.0,
                quadruped_medium::Species::Tarasque => 100.0,
                quadruped_medium::Species::Wolf => 130.0,
                quadruped_medium::Species::Frostfang => 115.0,
                quadruped_medium::Species::Mouflon => 75.0,
                quadruped_medium::Species::Catoblepas => 60.0,
                quadruped_medium::Species::Bonerattler => 115.0,
                quadruped_medium::Species::Deer => 120.0,
                quadruped_medium::Species::Hirdrasil => 110.0,
                quadruped_medium::Species::Roshwalr => 70.0,
                quadruped_medium::Species::Donkey => 90.0,
                quadruped_medium::Species::Camel => 75.0,
                quadruped_medium::Species::Zebra => 150.0,
                quadruped_medium::Species::Antelope => 155.0,
                quadruped_medium::Species::Kelpie => 140.0,
                quadruped_medium::Species::Horse => 140.0,
                quadruped_medium::Species::Barghest => 80.0,
                quadruped_medium::Species::Cattle => 80.0,
                quadruped_medium::Species::Darkhound => 115.0,
                quadruped_medium::Species::Highland => 80.0,
                quadruped_medium::Species::Yak => 80.0,
                quadruped_medium::Species::Panda => 90.0,
                quadruped_medium::Species::Bear => 90.0,
                quadruped_medium::Species::Dreadhorn => 95.0,
                quadruped_medium::Species::Moose => 105.0,
                quadruped_medium::Species::Snowleopard => 115.0,
                quadruped_medium::Species::Mammoth => 75.0,
                quadruped_medium::Species::Elephant => 75.0,
                quadruped_medium::Species::Ngoubou => 95.0,
                quadruped_medium::Species::Llama => 100.0,
                quadruped_medium::Species::Alpaca => 100.0,
                quadruped_medium::Species::Akhlut => 90.0,
                quadruped_medium::Species::Bristleback => 105.0,
                quadruped_medium::Species::ClaySteed => 85.0,
            },
            Body::BipedLarge(body) => match body.species {
                biped_large::Species::Slysaurok => 100.0,
                biped_large::Species::Occultsaurok => 100.0,
                biped_large::Species::Mightysaurok => 100.0,
                biped_large::Species::Mindflayer => 90.0,
                biped_large::Species::Minotaur => 60.0,
                biped_large::Species::Huskbrute => 130.0,
                biped_large::Species::Cultistwarlord => 110.0,
                biped_large::Species::Cultistwarlock => 90.0,
                biped_large::Species::Gigasfrost => 45.0,
                biped_large::Species::Gigasfire => 50.0,
                biped_large::Species::Forgemaster => 100.0,
                _ => 80.0,
            },
            Body::BirdMedium(_) => 80.0,
            Body::FishMedium(_) => 80.0,
            Body::Dragon(_) => 250.0,
            Body::BirdLarge(_) => 110.0,
            Body::FishSmall(_) => 60.0,
            Body::BipedSmall(biped_small) => match biped_small.species {
                biped_small::Species::Haniwa => 65.0,
                biped_small::Species::Boreal => 100.0,
                biped_small::Species::Gnarling => 70.0,
                _ => 80.0,
            },
            Body::Object(_) => 0.0,
            Body::Item(_) => 0.0,
            Body::Golem(body) => match body.species {
                golem::Species::ClayGolem => 120.0,
                golem::Species::IronGolem => 100.0,
                _ => 60.0,
            },
            Body::Theropod(theropod) => match theropod.species {
                theropod::Species::Archaeos
                | theropod::Species::Odonto
                | theropod::Species::Ntouka => 110.0,
                theropod::Species::Dodarock => 75.0,
                theropod::Species::Yale => 115.0,
                _ => 125.0,
            },
            Body::QuadrupedLow(quadruped_low) => match quadruped_low.species {
                quadruped_low::Species::Crocodile => 60.0,
                quadruped_low::Species::SeaCrocodile => 60.0,
                quadruped_low::Species::Alligator => 65.0,
                quadruped_low::Species::Salamander => 85.0,
                quadruped_low::Species::Elbst => 85.0,
                quadruped_low::Species::Monitor => 130.0,
                quadruped_low::Species::Asp => 100.0,
                quadruped_low::Species::Tortoise => 60.0,
                quadruped_low::Species::Rocksnapper => 70.0,
                quadruped_low::Species::Rootsnapper => 70.0,
                quadruped_low::Species::Reefsnapper => 70.0,
                quadruped_low::Species::Pangolin => 90.0,
                quadruped_low::Species::Maneater => 80.0,
                quadruped_low::Species::Sandshark => 125.0,
                quadruped_low::Species::Hakulaq => 125.0,
                quadruped_low::Species::Dagon => 140.0,
                quadruped_low::Species::Lavadrake => 100.0,
                quadruped_low::Species::Icedrake => 100.0,
                quadruped_low::Species::Basilisk => 85.0,
                quadruped_low::Species::Deadwood => 110.0,
                quadruped_low::Species::Mossdrake => 100.0,
                quadruped_low::Species::Driggle => 120.0,
                quadruped_low::Species::Snaretongue => 120.0,
                quadruped_low::Species::Hydra => 100.0,
            },
            Body::Ship(ship::Body::Carriage) => 40.0,
            Body::Ship(ship::Body::Train) => 9.0,
            Body::Ship(_) => 0.0,
            Body::Arthropod(arthropod) => match arthropod.species {
                arthropod::Species::Tarantula => 85.0,
                arthropod::Species::Blackwidow => 95.0,
                arthropod::Species::Antlion => 115.0,
                arthropod::Species::Hornbeetle => 80.0,
                arthropod::Species::Leafbeetle => 65.0,
                arthropod::Species::Stagbeetle => 80.0,
                arthropod::Species::Weevil => 70.0,
                arthropod::Species::Cavespider => 90.0,
                arthropod::Species::Moltencrawler => 70.0,
                arthropod::Species::Mosscrawler => 70.0,
                arthropod::Species::Sandcrawler => 70.0,
                arthropod::Species::Dagonite => 70.0,
                arthropod::Species::Emberfly => 75.0,
            },
            Body::Crustacean(body) => match body.species {
                crustacean::Species::Crab | crustacean::Species::SoldierCrab => 80.0,
                crustacean::Species::Karkatha => 120.0,
            },
            Body::Plugin(body) => body.base_accel(),
        }
    }

    pub fn air_accel(&self) -> f32 { self.base_accel() * 0.025 }

    /// Attempt to determine the maximum speed of the character
    /// when moving on the ground
    pub fn max_speed_approx(&self) -> f32 {
        let v = match self {
            Body::Ship(ship) => ship.get_speed(),
            // NOTE: that denominator evaluates to constant, at the time
            // of writing it's ~9.751134.
            //
            // We still have the formula here, for the sake of completeness,
            // and also for when we'll split FRIC_GROUND to be different
            // on the snow/ice/etc.
            _ => -self.base_accel() / (60.0 * (1.0 - FRIC_GROUND).ln()),
        };
        debug_assert!(v >= 0.0, "Speed must be positive!");
        v
    }

    /// How much orientation changes will be damped based on the severity of the
    /// turn.
    ///
    /// At 1.0, low-severity turns will be damped to a lower rate: this is more
    /// typical of the way bipedal creatures turn, for example. At 0.0, the
    /// turn rate is constant regardless of angle.
    pub fn ori_damping(&self) -> f32 {
        match self {
            Body::Humanoid(_) | Body::BipedLarge(_) | Body::Golem(_) => 1.0,
            _ => 0.0,
        }
    }

    /// The turn rate in 180°/s (or (rotations per second)/2)
    pub fn base_ori_rate(&self) -> f32 {
        match self {
            Body::Humanoid(body) => 2.65 / body.scaler(),
            Body::QuadrupedSmall(_) => 3.0,
            Body::QuadrupedMedium(quadruped_medium) => match quadruped_medium.species {
                quadruped_medium::Species::Mammoth => 1.0,
                _ => 2.8,
            },
            Body::BirdMedium(_) => 6.0,
            Body::FishMedium(_) => 6.0,
            Body::Dragon(_) => 1.0,
            Body::BirdLarge(_) => 7.0,
            Body::FishSmall(_) => 7.0,
            Body::BipedLarge(biped_large) => match biped_large.species {
                biped_large::Species::Harvester => 2.0,
                _ => 2.7,
            },
            Body::BipedSmall(_) => 3.5,
            Body::Object(_) => 2.0,
            Body::Item(_) => 2.0,
            Body::Golem(golem) => match golem.species {
                golem::Species::WoodGolem => 1.2,
                _ => 2.0,
            },
            Body::Theropod(theropod) => match theropod.species {
                theropod::Species::Archaeos => 2.3,
                theropod::Species::Odonto => 2.3,
                theropod::Species::Ntouka => 2.3,
                theropod::Species::Dodarock => 2.0,
                _ => 2.5,
            },
            Body::QuadrupedLow(quadruped_low) => match quadruped_low.species {
                quadruped_low::Species::Asp => 2.2,
                quadruped_low::Species::Tortoise => 1.5,
                quadruped_low::Species::Rocksnapper => 1.8,
                quadruped_low::Species::Rootsnapper => 1.8,
                quadruped_low::Species::Lavadrake => 1.7,
                quadruped_low::Species::Icedrake => 1.7,
                quadruped_low::Species::Mossdrake => 1.7,
                _ => 2.0,
            },
            Body::Ship(ship::Body::Carriage) => 0.04,
            Body::Ship(ship::Body::Train) => 0.0,
            Body::Ship(ship) if ship.has_water_thrust() => 5.0 / self.dimensions().y,
            Body::Ship(_) => 6.0 / self.dimensions().y,
            Body::Arthropod(_) => 3.5,
            Body::Crustacean(_) => 3.5,
            Body::Plugin(body) => body.base_ori_rate(),
        }
    }

    /// Returns thrust force if the body type can swim, otherwise None
    pub fn swim_thrust(&self) -> Option<f32> {
        // Swim thrust is proportional to the frontal area of the creature, since we
        // assume that strength roughly scales according to square laws. Also,
        // it happens to make balancing against drag much simpler.
        let front_profile = self.dimensions().x * self.dimensions().z;
        Some(
            match self {
                Body::Object(_) => return None,
                Body::Item(_) => return None,
                Body::Ship(ship::Body::Submarine) => 1000.0 * self.mass().0,
                Body::Ship(ship) if ship.has_water_thrust() => 500.0 * self.mass().0,
                Body::Ship(_) => return None,
                Body::BipedLarge(_) => 120.0 * self.mass().0,
                Body::Golem(_) => 100.0 * self.mass().0,
                Body::BipedSmall(_) => 1000.0 * self.mass().0,
                Body::BirdMedium(_) => 400.0 * self.mass().0,
                Body::BirdLarge(_) => 400.0 * self.mass().0,
                Body::FishMedium(_) => 200.0 * self.mass().0,
                Body::FishSmall(_) => 300.0 * self.mass().0,
                Body::Dragon(_) => 50.0 * self.mass().0,
                // Humanoids are a bit different: we try to give them thrusts that result in similar
                // speeds for gameplay reasons
                Body::Humanoid(body) => {
                    return Some(6_500_000.0 / self.mass().0 * body.scaler().powi(2));
                },
                Body::Theropod(body) => match body.species {
                    theropod::Species::Sandraptor
                    | theropod::Species::Snowraptor
                    | theropod::Species::Sunlizard
                    | theropod::Species::Woodraptor
                    | theropod::Species::Dodarock
                    | theropod::Species::Axebeak
                    | theropod::Species::Yale => 500.0 * self.mass().0,
                    _ => 150.0 * self.mass().0,
                },
                Body::QuadrupedLow(_) => 1200.0 * self.mass().0,
                Body::QuadrupedMedium(body) => match body.species {
                    quadruped_medium::Species::Mammoth => 150.0 * self.mass().0,
                    quadruped_medium::Species::Kelpie => 3500.0 * self.mass().0,
                    _ => 1000.0 * self.mass().0,
                },
                Body::QuadrupedSmall(_) => 1500.0 * self.mass().0,
                Body::Arthropod(_) => 500.0 * self.mass().0,
                Body::Crustacean(_) => 400.0 * self.mass().0,
                Body::Plugin(body) => body.swim_thrust()?,
            } * front_profile,
        )
    }

    /// Returns thrust force if the body type can fly, otherwise None
    pub fn fly_thrust(&self) -> Option<f32> {
        match self {
            Body::BirdMedium(body) => match body.species {
                bird_medium::Species::Bat | bird_medium::Species::BloodmoonBat => {
                    Some(GRAVITY * self.mass().0 * 0.5)
                },
                _ => Some(GRAVITY * self.mass().0 * 2.0),
            },
            Body::BirdLarge(_) => Some(GRAVITY * self.mass().0 * 0.5),
            Body::Dragon(_) => Some(200_000.0),
            Body::Ship(ship) if ship.can_fly() => Some(390_000.0),
            Body::Object(object::Body::Crux) => Some(1_000.0),
            _ => None,
        }
    }

    /// Returns whether the body uses vectored propulsion
    pub fn vectored_propulsion(&self) -> bool {
        match self {
            Body::Ship(ship) => ship.vectored_propulsion(),
            _ => false,
        }
    }

    /// Returns jump impulse if the body type can jump, otherwise None
    pub fn jump_impulse(&self) -> Option<f32> {
        match self {
            Body::Object(_) | Body::Ship(_) | Body::Item(_) => None,
            Body::BipedLarge(_) | Body::Dragon(_) => Some(0.6 * self.mass().0),
            Body::Golem(_) | Body::QuadrupedLow(_) => Some(0.4 * self.mass().0),
            Body::QuadrupedMedium(_) => Some(0.4 * self.mass().0),
            Body::Theropod(body) => match body.species {
                theropod::Species::Snowraptor
                | theropod::Species::Sandraptor
                | theropod::Species::Woodraptor => Some(0.4 * self.mass().0),
                _ => None,
            },
            Body::Arthropod(_) => Some(1.0 * self.mass().0),
            _ => Some(0.4 * self.mass().0),
        }
        .map(|f| f * GRAVITY)
    }

    pub fn can_climb(&self) -> bool { matches!(self, Body::Humanoid(_)) }

    /// Returns how well a body can move backwards while strafing (0.0 = not at
    /// all, 1.0 = same as forward)
    pub fn reverse_move_factor(&self) -> f32 { 0.45 }

    /// Returns the position where a projectile should be fired relative to this
    /// body
    pub fn projectile_offsets(&self, ori: Vec3<f32>, scale: f32) -> Vec3<f32> {
        let body_offsets_z = match self {
            Body::Golem(_) => self.height() * 0.4,
            _ => self.eye_height(scale),
        };

        let dim = self.dimensions();
        // The width (shoulder to shoulder) and length (nose to tail)
        let (width, length) = (dim.x, dim.y);
        let body_radius = if length > width {
            // Dachshund-like
            self.max_radius()
        } else {
            // Cyclops-like
            self.min_radius()
        };

        Vec3::new(
            body_radius * ori.x * 1.1,
            body_radius * ori.y * 1.1,
            body_offsets_z,
        )
    }
}

/// set footwear in idle data and potential state change to Skate
pub fn handle_skating(data: &JoinData, update: &mut StateUpdate) {
    if let &Idle(idle::Data {
        ref is_sneaking,
        ref time_entered,
        mut footwear,
    }) = data.character
    {
        if footwear.is_none() {
            footwear = data.inventory.and_then(|inv| {
                inv.equipped(EquipSlot::Armor(ArmorSlot::Feet))
                    .map(|armor| match armor.kind().as_ref() {
                        ItemKind::Armor(a) => {
                            a.stats(data.msm, armor.stats_durability_multiplier())
                                .ground_contact
                        },
                        _ => Friction::Normal,
                    })
            });
            update.character = Idle(idle::Data {
                is_sneaking: *is_sneaking,
                time_entered: *time_entered,
                footwear,
            });
        }
        if data.physics.skating_active {
            update.character =
                CharacterState::Skate(skate::Data::new(data, footwear.unwrap_or(Friction::Normal)));
        }
    }
}

/// Handles updating `Components` to move player based on state of `JoinData`
pub fn handle_move(data: &JoinData<'_>, update: &mut StateUpdate, efficiency: f32) {
    if data.volume_mount_data.is_some() {
        return;
    }
    let submersion = data
        .physics
        .in_liquid()
        .map(|depth| depth / data.body.height());

    if input_is_pressed(data, InputKind::Fly)
        && submersion.is_none_or(|sub| sub < 1.0)
        && (data.physics.on_ground.is_none() || data.body.jump_impulse().is_none())
        && data.body.fly_thrust().is_some()
    {
        fly_move(data, update, efficiency);
    } else if let Some(submersion) = (data.physics.in_liquid().is_some()
        && data.body.swim_thrust().is_some())
    .then_some(submersion)
    .flatten()
    {
        swim_move(data, update, efficiency, submersion);
    } else {
        basic_move(data, update, efficiency);
    }
}

/// Updates components to move player as if theyre on ground or in air
fn basic_move(data: &JoinData<'_>, update: &mut StateUpdate, efficiency: f32) {
    let section_modifier = match data.character.stage_section() {
        Some(StageSection::Buildup) => data.stats.buildup_move_speed_modifier,
        Some(StageSection::Charge) => data.stats.charge_move_speed_modifier,
        _ => 1.0,
    };
    let efficiency = efficiency
        * data.stats.move_speed_modifier
        * data.stats.friction_modifier
        * section_modifier;

    let accel = if let Some(block) = data.physics.on_ground {
        // FRIC_GROUND temporarily used to normalize things around expected values
        data.body.base_accel()
            * data.scale.map_or(1.0, |s| s.0.sqrt())
            * block.get_traction()
            * block.get_friction()
            / FRIC_GROUND
    } else {
        data.body.air_accel()
    } * efficiency;

    // Should ability to backpedal be separate from ability to strafe?
    update.vel.0 += Vec2::broadcast(data.dt.0)
        * accel
        * if data.body.can_strafe() {
            data.inputs.move_dir
                * if is_strafing(data, update) {
                    Lerp::lerp(
                        Vec2::from(update.ori)
                            .try_normalized()
                            .unwrap_or_else(Vec2::zero)
                            .dot(
                                data.inputs
                                    .move_dir
                                    .try_normalized()
                                    .unwrap_or_else(Vec2::zero),
                            )
                            .add(1.0)
                            .div(2.0)
                            .max(0.0),
                        1.0,
                        data.body.reverse_move_factor(),
                    )
                } else {
                    1.0
                }
        } else {
            let fw = Vec2::from(update.ori);
            fw * data.inputs.move_dir.dot(fw).max(0.0)
        };
}

/// Handles forced movement
pub fn handle_forced_movement(
    data: &JoinData<'_>,
    update: &mut StateUpdate,
    movement: ForcedMovement,
) {
    match movement {
        ForcedMovement::Forward(strength) => {
            let strength = strength * data.stats.move_speed_modifier * data.stats.friction_modifier;
            if let Some(accel) = data.physics.on_ground.map(|block| {
                // FRIC_GROUND temporarily used to normalize things around expected values
                data.body.base_accel() * block.get_traction() * block.get_friction() / FRIC_GROUND
            }) {
                update.vel.0 += Vec2::broadcast(data.dt.0)
                    * accel
                    * data.scale.map_or(1.0, |s| s.0.sqrt())
                    * Vec2::from(*data.ori)
                    * strength;
            }
        },
        ForcedMovement::Reverse(strength) => {
            let strength = strength * data.stats.move_speed_modifier * data.stats.friction_modifier;
            if let Some(accel) = data.physics.on_ground.map(|block| {
                // FRIC_GROUND temporarily used to normalize things around expected values
                data.body.base_accel() * block.get_traction() * block.get_friction() / FRIC_GROUND
            }) {
                update.vel.0 += Vec2::broadcast(data.dt.0)
                    * accel
                    * data.scale.map_or(1.0, |s| s.0.sqrt())
                    * -Vec2::from(*data.ori)
                    * strength;
            }
        },
        ForcedMovement::Sideways(strength) => {
            let strength = strength * data.stats.move_speed_modifier * data.stats.friction_modifier;
            if let Some(accel) = data.physics.on_ground.map(|block| {
                // FRIC_GROUND temporarily used to normalize things around expected values
                data.body.base_accel() * block.get_traction() * block.get_friction() / FRIC_GROUND
            }) {
                let direction = {
                    // Left if positive, else right
                    let side = Vec2::from(*data.ori)
                        .rotated_z(PI / 2.)
                        .dot(data.inputs.move_dir)
                        .signum();
                    if side > 0.0 {
                        Vec2::from(*data.ori).rotated_z(PI / 2.)
                    } else {
                        -Vec2::from(*data.ori).rotated_z(PI / 2.)
                    }
                };

                update.vel.0 += Vec2::broadcast(data.dt.0)
                    * accel
                    * data.scale.map_or(1.0, |s| s.0.sqrt())
                    * direction
                    * strength;
            }
        },
        ForcedMovement::DirectedReverse(strength) => {
            let strength = strength * data.stats.move_speed_modifier * data.stats.friction_modifier;
            if let Some(accel) = data.physics.on_ground.map(|block| {
                // FRIC_GROUND temporarily used to normalize things around expected values
                data.body.base_accel() * block.get_traction() * block.get_friction() / FRIC_GROUND
            }) {
                let direction = if Vec2::from(*data.ori).dot(data.inputs.move_dir).signum() > 0.0 {
                    data.inputs.move_dir.reflected(Vec2::from(*data.ori))
                } else {
                    data.inputs.move_dir
                }
                .try_normalized()
                .unwrap_or_else(|| -Vec2::from(*data.ori));
                update.vel.0 += direction * strength * accel * data.dt.0;
            }
        },
        ForcedMovement::AntiDirectedForward(strength) => {
            let strength = strength * data.stats.move_speed_modifier * data.stats.friction_modifier;
            if let Some(accel) = data.physics.on_ground.map(|block| {
                // FRIC_GROUND temporarily used to normalize things around expected values
                data.body.base_accel() * block.get_traction() * block.get_friction() / FRIC_GROUND
            }) {
                let direction = if Vec2::from(*data.ori).dot(data.inputs.move_dir).signum() < 0.0 {
                    data.inputs.move_dir.reflected(Vec2::from(*data.ori))
                } else {
                    data.inputs.move_dir
                }
                .try_normalized()
                .unwrap_or_else(|| Vec2::from(*data.ori));
                let direction = direction.reflected(Vec2::from(*data.ori).rotated_z(PI / 2.));
                update.vel.0 += direction * strength * accel * data.dt.0;
            }
        },
        ForcedMovement::Leap {
            vertical,
            forward,
            progress,
            direction,
        } => {
            let dir = direction.get_2d_dir(data);
            // Apply jumping force
            update.vel.0 = Vec3::new(
                dir.x,
                dir.y,
                vertical,
            )
                * data.scale.map_or(1.0, |s| s.0.sqrt())
                // Multiply decreasing amount linearly over time (with average of 1)
                * 2.0 * progress
                // Apply direction
                + Vec3::from(dir)
                // Multiply by forward leap strength
                * forward
                // Control forward movement based on look direction.
                // This allows players to stop moving forward when they
                // look downward at target
                * (1.0 - data.inputs.look_dir.z.abs());
        },
    }
}

pub fn handle_orientation(
    data: &JoinData<'_>,
    update: &mut StateUpdate,
    efficiency: f32,
    dir_override: Option<Dir>,
) {
    /// first check for horizontal
    fn to_horizontal_fast(ori: &crate::comp::Ori) -> crate::comp::Ori {
        if ori.to_quat().into_vec4().xy().is_approx_zero() {
            *ori
        } else {
            ori.to_horizontal()
        }
    }
    /// compute an upper limit for the difference of two orientations
    fn ori_absdiff(a: &crate::comp::Ori, b: &crate::comp::Ori) -> f32 {
        (a.to_quat().into_vec4() - b.to_quat().into_vec4()).reduce(|a, b| a.abs() + b.abs())
    }

    // Look at things
    update.character_activity.look_dir = Some(data.controller.inputs.look_dir);

    let (tilt_ori, efficiency) = if let Body::Ship(ship) = data.body
        && ship.has_wheels()
    {
        let height_at = |rpos| {
            data.terrain
                .ray(
                    data.pos.0 + rpos + Vec3::unit_z() * 4.0,
                    data.pos.0 + rpos - Vec3::unit_z() * 4.0,
                )
                .until(Block::is_solid)
                .cast()
                .0
        };

        // Do some cheap raycasting with the ground to determine the appropriate
        // orientation for the vehicle
        let x_diff = (height_at(data.ori.to_horizontal().right().to_vec() * 3.0)
            - height_at(data.ori.to_horizontal().right().to_vec() * -3.0))
            / 10.0;
        let y_diff = (height_at(data.ori.to_horizontal().look_dir().to_vec() * -4.5)
            - height_at(data.ori.to_horizontal().look_dir().to_vec() * 4.5))
            / 10.0;

        (
            Quaternion::rotation_y(x_diff.atan()) * Quaternion::rotation_x(y_diff.atan()),
            (data.vel.0 - data.physics.ground_vel)
                .xy()
                .magnitude()
                .max(3.0)
                * efficiency,
        )
    } else {
        (Quaternion::identity(), efficiency)
    };

    // Direction is set to the override if one is provided, else if entity is
    // strafing or attacking the horiontal component of the look direction is used,
    // else we special-case talking, else the current horizontal movement direction
    // is used
    let target_ori = if let Some(dir_override) = dir_override {
        dir_override.into()
    } else if let CharacterState::Talk(t) = data.character
        && let Some(tgt_uid) = t.tgt
        && let Some(tgt) = data.id_maps.uid_entity(tgt_uid)
        && let (tgt_body, Some(tgt_prev_phys)) =
            (data.bodies.get(tgt), data.prev_phys_caches.get(tgt))
        && let Some(tgt_pos) = tgt_prev_phys.pos.as_ref()
        && let Some(dir) = Dir::look_toward(
            data.pos,
            Some(data.body),
            data.scale,
            tgt_pos,
            tgt_body,
            Some(&Scale(tgt_prev_phys.scale)),
        )
    {
        update.character_activity.look_dir = Some(dir);
        Dir::to_horizontal(dir).unwrap_or(dir).into()
    } else if is_strafing(data, update) || update.character.should_follow_look() {
        data.inputs
            .look_dir
            .to_horizontal()
            .unwrap_or_default()
            .into()
    } else {
        Dir::from_unnormalized(data.inputs.move_dir.into())
            .map_or_else(|| to_horizontal_fast(data.ori), |dir| dir.into())
    }
    .rotated(tilt_ori);
    // unit is multiples of 180°
    let half_turns_per_tick = data.body.base_ori_rate() / data.scale.map_or(1.0, |s| s.0.sqrt())
        * efficiency
        * if data.physics.in_liquid().is_some() {
            0.4
        } else if data.physics.on_ground.is_some() || data.mount_data.is_some() {
            1.0
        } else {
            0.2
        }
        * data.dt.0;
    // very rough guess
    let ticks_from_target_guess = ori_absdiff(&update.ori, &target_ori) / half_turns_per_tick;
    let instantaneous = ticks_from_target_guess < 1.0;
    update.ori = if data.volume_mount_data.is_some() {
        update.ori
    } else if instantaneous {
        target_ori
    } else {
        let target_fraction = {
            // Angle factor used to keep turning rate approximately constant by
            // counteracting slerp turning more with a larger angle
            let angle_factor =
                2.0 / (1.0 - update.ori.dot(target_ori) * (1.0 - data.body.ori_damping())).sqrt();

            half_turns_per_tick * angle_factor
        };
        update
            .ori
            .slerped_towards(target_ori, target_fraction.min(1.0))
    };
}

/// Updates components to move player as if theyre swimming
fn swim_move(
    data: &JoinData<'_>,
    update: &mut StateUpdate,
    efficiency: f32,
    submersion: f32,
) -> bool {
    let efficiency = efficiency * data.stats.swim_speed_modifier * data.stats.friction_modifier;
    if let Some(force) = data.body.swim_thrust() {
        let force = efficiency * force * data.scale.map_or(1.0, |s| s.0);
        let mut water_accel = force / data.mass.0;

        if let Ok(level) = data.skill_set.skill_level(Skill::Swim(SwimSkill::Speed)) {
            let modifiers = SKILL_MODIFIERS.general_tree.swim;
            water_accel *= modifiers.speed.powi(level.into());
        }

        let dir = if data.body.can_strafe() {
            data.inputs.move_dir
        } else {
            let fw = Vec2::from(update.ori);
            fw * data.inputs.move_dir.dot(fw).max(0.0)
        };

        // Automatically tread water to stay afloat
        let move_z = if submersion < 1.0
            && data.inputs.move_z.abs() < f32::EPSILON
            && data.physics.on_ground.is_none()
        {
            submersion.max(0.0) * 0.1
        } else {
            data.inputs.move_z
        };

        // Assume that feet/flippers get less efficient as we become less submerged
        let move_z = move_z.min((submersion * 1.5 - 0.5).clamp(0.0, 1.0).powi(2));

        update.vel.0 += Vec3::new(dir.x, dir.y, move_z)
                // TODO: Should probably be normalised, but creates odd discrepancies when treading water
                // .try_normalized()
                // .unwrap_or_default()
            * water_accel
            // Gives a good balance between submerged and surface speed
            * submersion.clamp(0.0, 1.0).sqrt()
            // Good approximate compensation for dt-dependent effects
            * data.dt.0 * 0.04;

        true
    } else {
        false
    }
}

/// Updates components to move entity as if it's flying
pub fn fly_move(data: &JoinData<'_>, update: &mut StateUpdate, efficiency: f32) -> bool {
    let efficiency = efficiency * data.stats.move_speed_modifier * data.stats.friction_modifier;

    let glider = match data.character {
        CharacterState::Glide(data) => Some(data),
        _ => None,
    };
    if let Some(force) = data
        .body
        .fly_thrust()
        .or_else(|| glider.is_some().then_some(0.0))
    {
        let thrust = efficiency * force;
        let accel = thrust / data.mass.0;

        match data.body {
            Body::Ship(ship::Body::DefaultAirship) => {
                // orient the airship according to the controller look_dir
                // Make the airship rotation more efficient (x2) so that it
                // can orient itself more quickly.
                handle_orientation(
                    data,
                    update,
                    efficiency * 2.0,
                    Some(data.controller.inputs.look_dir),
                );
            },
            _ => {
                handle_orientation(data, update, efficiency, None);
            },
        }

        let mut update_fw_vel = true;
        // Elevation control
        match data.body {
            // flappy flappy
            Body::Dragon(_) | Body::BirdLarge(_) | Body::BirdMedium(_) => {
                let anti_grav = GRAVITY * (1.0 + data.inputs.move_z.min(0.0));
                update.vel.0.z += data.dt.0 * (anti_grav + accel * data.inputs.move_z.max(0.0));
            },
            // led zeppelin
            Body::Ship(ship::Body::DefaultAirship) => {
                update_fw_vel = false;
                // airships or zeppelins are controlled by their engines and should have
                // neutral buoyancy. Don't change their density.
                // Assume that the airship is always level and that the engines are gimbaled
                // so that they can provide thrust in any direction.
                // The vector of thrust is the desired movement direction scaled by the
                // acceleration.
                let thrust_dir = data.inputs.move_dir.with_z(data.inputs.move_z);
                update.vel.0 += thrust_dir * data.dt.0 * accel;
            },
            // floaty floaty
            Body::Ship(ship) if ship.can_fly() => {
                // Balloons gain altitude by modifying their density, e.g. by heating the air
                // inside. Ships float by adjusting their buoyancy, e.g. by
                // pumping water in or out. Simulate a ship or balloon by
                // adjusting its density.
                let regulate_density = |min: f32, max: f32, def: f32, rate: f32| -> Density {
                    // Reset to default on no input
                    let change = if data.inputs.move_z.abs() > f32::EPSILON {
                        -data.inputs.move_z
                    } else {
                        (def - data.density.0).clamp(-1.0, 1.0)
                    };
                    Density((update.density.0 + data.dt.0 * rate * change).clamp(min, max))
                };
                let def_density = ship.density().0;
                if data.physics.in_liquid().is_some() {
                    let hull_density = ship.hull_density().0;
                    update.density.0 =
                        regulate_density(def_density * 0.6, hull_density, hull_density, 25.0).0;
                } else {
                    update.density.0 =
                        regulate_density(def_density * 0.5, def_density * 1.5, def_density, 0.5).0;
                };
            },
            // oopsie woopsie
            // TODO: refactor to make this state impossible
            _ => {},
        };

        if update_fw_vel {
            update.vel.0 += Vec2::broadcast(data.dt.0)
                * accel
                * if data.body.can_strafe() {
                    data.inputs.move_dir
                } else {
                    let fw = Vec2::from(update.ori);
                    fw * data.inputs.move_dir.dot(fw).max(0.0)
                };
        }
        true
    } else {
        false
    }
}

/// Checks if an input related to an attack is held. If one is, moves entity
/// into wielding state
pub fn handle_wield(data: &JoinData<'_>, update: &mut StateUpdate) {
    if data.controller.queued_inputs.keys().any(|i| i.is_ability()) {
        attempt_wield(data, update);
    }
}

/// If a tool is equipped, goes into Equipping state, otherwise goes to Idle
pub fn attempt_wield(data: &JoinData<'_>, update: &mut StateUpdate) {
    // Closure to get equip time provided an equip slot if a tool is equipped in
    // equip slot
    let equip_time = |equip_slot| {
        data.inventory
            .and_then(|inv| inv.equipped(equip_slot))
            .and_then(|item| match &*item.kind() {
                ItemKind::Tool(tool) => Some(Duration::from_secs_f32(
                    tool.stats(item.stats_durability_multiplier())
                        .equip_time_secs,
                )),
                _ => None,
            })
    };

    // Calculates time required to equip weapons, if weapon in mainhand and offhand,
    // uses maximum duration
    let mainhand_equip_time = equip_time(EquipSlot::ActiveMainhand);
    let offhand_equip_time = equip_time(EquipSlot::ActiveOffhand);
    let equip_time = match (mainhand_equip_time, offhand_equip_time) {
        (Some(a), Some(b)) => Some(a.max(b)),
        (Some(a), None) | (None, Some(a)) => Some(a),
        (None, None) => None,
    };

    // Moves entity into equipping state if there is some equip time, else moves
    // instantly into wield
    if let Some(equip_time) = equip_time {
        update.character = CharacterState::Equipping(equipping::Data {
            static_data: equipping::StaticData {
                buildup_duration: equip_time,
            },
            timer: Duration::default(),
            is_sneaking: update.character.is_stealthy(),
        });
    } else {
        update.character = CharacterState::Wielding(wielding::Data {
            is_sneaking: update.character.is_stealthy(),
        });
    }
}

/// Checks that player can `Sit` and updates `CharacterState` if so
pub fn attempt_sit(data: &JoinData<'_>, update: &mut StateUpdate) {
    if data.physics.on_ground.is_some() {
        update.character = CharacterState::Sit;
    }
}

/// Checks that player can `Crawl` and updates `CharacterState` if so
pub fn attempt_crawl(data: &JoinData<'_>, update: &mut StateUpdate) {
    if data.physics.on_ground.is_some() {
        update.character = CharacterState::Crawl;
    }
}

pub fn attempt_dance(data: &JoinData<'_>, update: &mut StateUpdate) {
    if data.physics.on_ground.is_some() && data.body.is_humanoid() {
        update.character = CharacterState::Dance;
    }
}

pub fn can_perform_pet(position: Pos, target_position: Pos, target_alignment: Alignment) -> bool {
    let within_distance = position.0.distance_squared(target_position.0) <= MAX_MOUNT_RANGE.powi(2);
    let valid_alignment = matches!(target_alignment, Alignment::Owned(_) | Alignment::Tame);

    within_distance && valid_alignment
}

pub fn attempt_talk(data: &JoinData<'_>, update: &mut StateUpdate, tgt: Option<Uid>) {
    if data.physics.on_ground.is_some() {
        update.character = CharacterState::Talk(match update.character {
            CharacterState::Talk(t) if t.tgt == tgt => t.refreshed(),
            _ => talk::Data::at(tgt),
        });
    }
}

pub fn attempt_sneak(data: &JoinData<'_>, update: &mut StateUpdate) {
    if data.physics.on_ground.is_some() && data.body.is_humanoid() {
        update.character = Idle(idle::Data {
            is_sneaking: true,
            time_entered: *data.time,
            footwear: data.character.footwear(),
        });
    }
}

/// Checks that player can `Climb` and updates `CharacterState` if so
pub fn handle_climb(data: &JoinData<'_>, update: &mut StateUpdate) -> bool {
    let Some(wall_dir) = data.physics.on_wall else {
        return false;
    };

    let towards_wall = data.inputs.move_dir.dot(wall_dir.xy()) > 0.0;
    // Only allow climbing if we are near the surface
    let underwater = data
        .physics
        .in_liquid()
        .map(|depth| depth > 2.0)
        .unwrap_or(false);
    let can_climb = data.body.can_climb() || data.physics.in_liquid().is_some();
    let in_air = data.physics.on_ground.is_none();
    if towards_wall && in_air && !underwater && can_climb && update.energy.current() > 1.0 {
        update.character = CharacterState::Climb(
            climb::Data::create_adjusted_by_skills(data)
                .with_wielded(data.character.is_wield() || data.character.was_wielded()),
        );
        true
    } else {
        false
    }
}

pub fn handle_wallrun(data: &JoinData<'_>, update: &mut StateUpdate) -> bool {
    if data.physics.on_wall.is_some()
        && data.physics.on_ground.is_none()
        && data.physics.in_liquid().is_none()
        && data.body.can_climb()
    {
        update.character = CharacterState::Wallrun(wallrun::Data {
            was_wielded: data.character.is_wield() || data.character.was_wielded(),
        });
        true
    } else {
        false
    }
}
/// Checks that player can Swap Weapons and updates `Loadout` if so
pub fn attempt_swap_equipped_weapons(
    data: &JoinData<'_>,
    update: &mut StateUpdate,
    output_events: &mut OutputEvents,
) {
    if data
        .inventory
        .and_then(|inv| inv.equipped(EquipSlot::InactiveMainhand))
        .is_some()
        || data
            .inventory
            .and_then(|inv| inv.equipped(EquipSlot::InactiveOffhand))
            .is_some()
    {
        update.swap_equipped_weapons = true;
        loadout_change_hook(data, output_events, false);
    }
}

/// Checks if a block can be reached from a position.
fn can_reach_block(
    player_pos: Vec3<f32>,
    block_pos: Vec3<i32>,
    range: f32,
    body: &Body,
    terrain: &TerrainGrid,
) -> bool {
    let block_pos_f32 = block_pos.map(|x| x as f32 + 0.5);
    // Closure to check if distance between a point and the block is less than
    // range and the radius of the body
    let block_range_check = |pos: Vec3<f32>| {
        (block_pos_f32 - pos).magnitude_squared() < (range + body.max_radius()).powi(2)
    };

    // Checks if player's feet or head is near to block
    let close_to_block = block_range_check(player_pos)
        || block_range_check(player_pos + Vec3::new(0.0, 0.0, body.height()));
    if close_to_block {
        // Do a check that a path can be found between sprite and entity
        // interacting with sprite Use manhattan distance * 1.5 for number
        // of iterations
        let iters = (3.0 * (block_pos_f32 - player_pos).map(|x| x.abs()).sum()) as usize;
        // Heuristic compares manhattan distance of start and end pos
        let heuristic = move |pos: &Vec3<i32>| (block_pos - pos).map(|x| x.abs()).sum() as f32;

        let mut astar = Astar::new(
            iters,
            player_pos.map(|x| x.floor() as i32),
            BuildHasherDefault::<FxHasher64>::default(),
        );

        // Transition uses manhattan distance as the cost, with a slightly lower cost
        // for z transitions
        let transition = |a: Vec3<i32>, b: Vec3<i32>| {
            let (a, b) = (a.map(|x| x as f32), b.map(|x| x as f32));
            ((a - b) * Vec3::new(1.0, 1.0, 0.9)).map(|e| e.abs()).sum()
        };
        // Neighbors are all neighboring blocks that are air
        let neighbors = |pos: &Vec3<i32>| {
            const DIRS: [Vec3<i32>; 6] = [
                Vec3::new(1, 0, 0),
                Vec3::new(-1, 0, 0),
                Vec3::new(0, 1, 0),
                Vec3::new(0, -1, 0),
                Vec3::new(0, 0, 1),
                Vec3::new(0, 0, -1),
            ];
            let pos = *pos;
            DIRS.iter()
                .map(move |dir| {
                    let dest = dir + pos;
                    (dest, transition(pos, dest))
                })
                .filter(|(pos, _)| {
                    terrain
                        .get(*pos)
                        .ok()
                        .is_some_and(|block| !block.is_filled())
                })
        };
        // Pathing satisfied when it reaches the sprite position
        let satisfied = |pos: &Vec3<i32>| *pos == block_pos;

        astar
            .poll(iters, heuristic, neighbors, satisfied)
            .into_path()
            .is_some()
    } else {
        false
    }
}

/// Handles inventory manipulations that affect the loadout
pub fn handle_manipulate_loadout(
    data: &JoinData<'_>,
    output_events: &mut OutputEvents,
    update: &mut StateUpdate,
    inv_action: InventoryAction,
) {
    // Trigger the hook for everything except the Collect action so that buffs
    // and combos are preserved.
    if !matches!(inv_action, InventoryAction::Collect(_)) {
        loadout_change_hook(data, output_events, true);
    }
    match inv_action {
        InventoryAction::Use(slot @ Slot::Inventory(inv_slot)) => {
            // If inventory action is using a slot, and slot is in the inventory
            // TODO: Do some non lazy way of handling the possibility that items equipped in
            // the loadout will have effects that are desired to be non-instantaneous
            use use_item::ItemUseKind;
            if let Some((item_kind, item)) = data
                .inventory
                .and_then(|inv| inv.get(inv_slot))
                .and_then(|item| Option::<ItemUseKind>::from(&*item.kind()).zip(Some(item)))
            {
                let (buildup_duration, use_duration, recover_duration) = item_kind.durations();
                // If item returns a valid kind for item use, do into use item character state
                update.character = CharacterState::UseItem(use_item::Data {
                    static_data: use_item::StaticData {
                        buildup_duration,
                        use_duration,
                        recover_duration,
                        inv_slot,
                        item_kind,
                        item_hash: item.item_hash(),
                        was_wielded: data.character.is_wield(),
                        was_sneak: data.character.is_stealthy(),
                    },
                    timer: Duration::default(),
                    stage_section: StageSection::Buildup,
                });
            } else {
                // Else emit inventory action instantaneously
                let inv_manip = InventoryManip::Use(slot);
                output_events.emit_server(InventoryManipEvent(data.entity, inv_manip));
            }
        },
        InventoryAction::Collect(sprite_pos) => {
            // First, get sprite data for position, if there is a sprite
            let sprite_at_pos = data
                .terrain
                .get(sprite_pos)
                .ok()
                .copied()
                .and_then(|b| b.get_sprite());
            // Checks if position has a collectible sprite as well as what sprite is at the
            // position
            let sprite_interact =
                sprite_at_pos.and_then(Option::<interact::SpriteInteractKind>::from);
            if let Some(sprite_interact) = sprite_interact
                && can_reach_block(
                    data.pos.0,
                    sprite_pos,
                    MAX_PICKUP_RANGE,
                    data.body,
                    data.terrain,
                )
            {
                let sprite_cfg = data.terrain.sprite_cfg_at(sprite_pos);
                let required_item = sprite_at_pos.and_then(|s| {
                    s.unlock_condition(sprite_cfg)
                        .and_then(|unlock| match unlock.into_owned() {
                            UnlockKind::Free => None,
                            UnlockKind::Requires(item) => Some((item, false)),
                            UnlockKind::Consumes(item) => Some((item, true)),
                        })
                });
                // None: An required items exist but no available
                // Some(None): No required items
                // Some(Some(_)): Required items satisfied, contains info about them
                let has_required_items = match required_item {
                    // Produces `None` if we can't find the item or `Some(Some(_))` if we can
                    Some((item_id, consume)) => data
                        .inventory
                        .and_then(|inv| inv.get_slot_of_item_by_def_id(&item_id))
                        .map(|slot| Some((item_id, slot, consume))),
                    None => Some(None),
                };
                if let Some(required_item) = has_required_items {
                    // If the sprite is collectible, enter the sprite interaction character
                    // state TODO: Handle cases for sprite being
                    // interactible, but not collectible (none currently
                    // exist)
                    let (buildup_duration, use_duration, recover_duration) =
                        sprite_interact.durations();

                    update.character = CharacterState::Interact(interact::Data {
                        static_data: interact::StaticData {
                            buildup_duration,
                            // Item interactions are never indefinite
                            use_duration: Some(use_duration),
                            recover_duration,
                            interact: interact::InteractKind::Sprite {
                                pos: sprite_pos,
                                kind: sprite_interact,
                            },
                            was_wielded: data.character.is_wield(),
                            was_sneak: data.character.is_stealthy(),
                            required_item,
                        },
                        timer: Duration::default(),
                        stage_section: StageSection::Buildup,
                    })
                } else {
                    output_events.emit_local(LocalEvent::CreateOutcome(
                        Outcome::FailedSpriteUnlock { pos: sprite_pos },
                    ));
                }
            }
        },
        // For inventory actions without a dedicated character state, just do action instantaneously
        InventoryAction::Swap(equip, slot) => {
            let inv_manip = InventoryManip::Swap(Slot::Equip(equip), slot);
            output_events.emit_server(InventoryManipEvent(data.entity, inv_manip));
        },
        InventoryAction::Drop(equip) => {
            let inv_manip = InventoryManip::Drop(Slot::Equip(equip));
            output_events.emit_server(InventoryManipEvent(data.entity, inv_manip));
        },
        InventoryAction::Sort(sort_order) => {
            output_events.emit_server(InventoryManipEvent(
                data.entity,
                InventoryManip::Sort(sort_order),
            ));
        },
        InventoryAction::Use(slot @ Slot::Equip(_)) => {
            let inv_manip = InventoryManip::Use(slot);
            output_events.emit_server(InventoryManipEvent(data.entity, inv_manip));
        },
        InventoryAction::Use(Slot::Overflow(_)) => {
            // Items in overflow slots cannot be used until moved to a real slot
        },
        InventoryAction::ToggleSpriteLight(pos, enable) => {
            if matches!(pos.kind, Volume::Terrain) {
                let sprite_interact = interact::SpriteInteractKind::ToggleLight(enable);

                let (buildup_duration, use_duration, recover_duration) =
                    sprite_interact.durations();

                update.character = CharacterState::Interact(interact::Data {
                    static_data: interact::StaticData {
                        buildup_duration,
                        use_duration: Some(use_duration),
                        recover_duration,
                        interact: interact::InteractKind::Sprite {
                            pos: pos.pos,
                            kind: sprite_interact,
                        },
                        was_wielded: data.character.is_wield(),
                        was_sneak: data.character.is_stealthy(),
                        required_item: None,
                    },
                    timer: Duration::default(),
                    stage_section: StageSection::Buildup,
                });
            }
        },
    }
}

/// Checks that player can wield the glider and updates `CharacterState` if so
pub fn attempt_glide_wield(
    data: &JoinData<'_>,
    update: &mut StateUpdate,
    output_events: &mut OutputEvents,
) {
    if data
        .inventory
        .and_then(|inv| inv.equipped(EquipSlot::Glider))
        .is_some()
        && !data
            .physics
            .in_liquid()
            .map(|depth| depth > 1.0)
            .unwrap_or(false)
        && data.body.is_humanoid()
        && data.mount_data.is_none()
        && data.volume_mount_data.is_none()
    {
        output_events.emit_local(LocalEvent::CreateOutcome(Outcome::Glider {
            pos: data.pos.0,
            wielded: true,
        }));
        update.character = CharacterState::GlideWield(glide_wield::Data::from(data));
    }
}

/// Checks that player can jump and sends jump event if so
pub fn handle_jump(
    data: &JoinData<'_>,
    output_events: &mut OutputEvents,
    _update: &mut StateUpdate,
    strength: f32,
) -> bool {
    input_is_pressed(data, InputKind::Jump)
        .then(|| data.body.jump_impulse())
        .flatten()
        .and_then(|impulse| {
            if data.physics.in_liquid().is_some() {
                if data.physics.on_wall.is_some() {
                    // Allow entities to make a small jump when at the edge of a body of water,
                    // allowing them to path out of it
                    Some(impulse * 0.75)
                } else {
                    None
                }
            } else if data.physics.on_ground.is_some() {
                Some(impulse)
            } else {
                None
            }
        })
        .map(|impulse| {
            output_events.emit_local(LocalEvent::Jump(
                data.entity,
                strength * impulse / data.mass.0
                    * data.scale.map_or(1.0, |s| s.0.powf(13.0).powf(0.25))
                    * data.stats.jump_modifier,
            ));
        })
        .is_some()
}

pub fn handle_walljump(
    data: &JoinData<'_>,
    output_events: &mut OutputEvents,
    update: &mut StateUpdate,
    was_wielded: bool,
) -> bool {
    let Some(wall_dir) = data.physics.on_wall else {
        return false;
    };
    const WALL_JUMP_Z: f32 = 0.7;
    let look_dir = data.inputs.look_dir.vec();

    // If looking at wall jump into look direction reflected off of the wall
    let jump_dir = if look_dir.xy().dot(wall_dir.xy()) > 0.0 {
        look_dir.xy().reflected(-wall_dir.xy()).with_z(WALL_JUMP_Z)
    } else {
        *look_dir
    };

    // If there is move input while walljumping favour the input direction
    let jump_dir = if data.inputs.move_dir.dot(-wall_dir.xy()) > 0.0 {
        data.inputs.move_dir.with_z(WALL_JUMP_Z)
    } else {
        jump_dir
    };

    // Prevent infinite upwards jumping
    let jump_dir = if jump_dir.xy().iter().all(|e| *e < 0.001) {
        jump_dir - wall_dir.xy() * 0.1
    } else {
        jump_dir
    }
    .try_normalized()
    .unwrap_or(Vec3::zero());

    if let Some(jump_impulse) = data.body.jump_impulse() {
        // Update orientation to look towards jump direction
        update.ori = update
            .ori
            .slerped_towards(Ori::from(Dir::new(jump_dir)), 20.0);
        // How strong the climb boost is relative to a normal jump
        const WALL_JUMP_FACTOR: f32 = 1.1;
        // Apply force
        output_events.emit_local(LocalEvent::ApplyImpulse {
            entity: data.entity,
            impulse: jump_dir * WALL_JUMP_FACTOR * jump_impulse / data.mass.0
                * data.scale.map_or(1.0, |s| s.0.powf(13.0).powf(0.25)),
        });
    }
    if was_wielded {
        update.character = CharacterState::Wielding(wielding::Data { is_sneaking: false });
    } else {
        update.character = CharacterState::Idle(idle::Data::default());
    }
    true
}

/// The one-shot authorisation a firing trigger slot holds, if `input` names a
/// slot that currently holds one.
///
/// The token is server-minted state living in `TriggerSlots`; nothing a client
/// can send produces it. `InputKind::TriggerAbility(i)` says *which slot*,
/// never *that it is permitted*.
fn trigger_token<'a>(data: &'a JoinData<'_>, input: InputKind) -> Option<&'a str> {
    match input {
        InputKind::TriggerAbility(slot) => data
            .trigger_slots
            .and_then(|slots| slots.firing_token(usize::from(slot))),
        _ => None,
    }
}

fn handle_ability(
    data: &JoinData<'_>,
    update: &mut StateUpdate,
    output_events: &mut OutputEvents,
    input: InputKind,
) -> bool {
    // A `TriggerAbility` input is honoured only while its slot actually holds a
    // live authorisation token. Any other one — a stale queued input left over
    // after the slot went back to `Ready`, or a crafted packet that somehow got
    // past the message-boundary deny — resolves to nothing at all.
    let token = trigger_token(data, input);
    if matches!(input, InputKind::TriggerAbility(_)) && token.is_none() {
        return false;
    }

    if let Some(ability_input) = input.into()
        && let Some((ability, from_offhand, spec_ability)) = data
            .active_abilities
            .and_then(|a| {
                a.activate_ability(
                    ability_input,
                    data.inventory,
                    data.attuned,
                    data.skill_set,
                    Some(data.body),
                    Some(data.character),
                    data.stance,
                    data.combo,
                    Some(data.stats),
                    data.buffs,
                    data.ability_pool,
                    data.character_class,
                    data.trigger_slots,
                    data.ability_map,
                )
            })
            .map(|(mut a, f, s)| {
                let mut contextual_stats =
                    if let Some(contextual_stats) = a.ability_meta().contextual_stats {
                        contextual_stats.equivalent_stats(data)
                    } else {
                        tool::Stats::one()
                    };
                contextual_stats.energy_efficiency *= data.stats.energy_efficiency_modifier;
                a = a.adjusted_by_stats(contextual_stats);
                (a, f, s)
            })
            .filter(|(ability, _, spec_ability)| {
                // 🔴 The ONE privilege a trigger buys, and the only one:
                // `cooldown_ready` is skipped. Everything below this line still
                // runs, unmodified.
                (cooldown_bypassed(data, token, spec_ability)
                    || cooldown_ready(data, ability, spec_ability))
                    && hp_cost_affordable(
                        ability.ability_meta().hp_cost,
                        data.health.map(|h| h.current()),
                        data.hardcore,
                    )
                    && ability.requirements_paid(data, update)
                    // BL-36: an antimagic field blocks magic abilities (those with a
                    // magic `source`); physical + innate abilities (source: None) pass.
                    && !(data.stats.disable_magic && ability.ability_meta().source.is_some())
                    // Per-spell class filter: a spell is castable only by the
                    // classes its own compendium `classes` list names. This is
                    // the ONLY class-side restriction on casting — a spell's
                    // `source` records where its magic comes from and never
                    // narrows who may cast it. Uncatalogued abilities and
                    // entities without a `CharacterClass` are exempt; see
                    // `SpellCompendium::allows`'s own doc comment.
                    && spec_ability
                        .ability_id(Some(data.character), data.inventory, data.ability_pool)
                        .is_none_or(|id| {
                            spell_compendium_manifest().allows(id, data.character_class)
                        })
                    // Possession gate: independent of the class filter above
                    // (both happen to do a compendium lookup, so they are
                    // kept adjacent) -- casting a levelled spell additionally
                    // requires a Tome in hand, regardless of who is
                    // permitted to know it.
                    && tome_possession_ok(
                        data.inventory,
                        data.character_class,
                        spec_ability.ability_id(
                            Some(data.character),
                            data.inventory,
                            data.ability_pool,
                        ),
                    )
            })
    {
        // TODO: Change requirements_paid to requirements_met, and then pay requirements
        // here (necessary after energy and combo moved to AbilityMeta)
        let ability_meta = ability.ability_meta();
        {
            let AbilityRequirements {
                stance: _,
                item,
                oracle: _,
                min_level: _,
            } = ability_meta.requirements;
            let inv_slot = item.and_then(|item| {
                data.inventory
                    .and_then(|inv| inv.get_slot_of_item_by_def_id(&item.item_def_id()))
            });
            if let Some(inv_slot) = inv_slot {
                let inv_manip = InventoryManip::Delete(
                    inv_slot,
                    NonZeroU32::new(1).expect("1 is greater than 0"),
                );
                output_events.emit_server(InventoryManipEvent(data.entity, inv_manip));
            }
        }
        let spec_ability_copy = spec_ability;
        match CharacterState::try_from((
            &ability,
            AbilityInfo::new(data, from_offhand, input, Some(spec_ability), ability_meta),
            data,
        )) {
            Ok(character_state) => {
                let tool_kind = character_state.ability_info().and_then(|ai| ai.tool);
                let target_uid = character_state
                    .ability_info()
                    .and_then(|ai| ai.input_attr)
                    .and_then(|ia| ia.target_entity);
                update.character = character_state;

                // 🔴 The bypass is symmetric — it skips the read AND the write:
                //
                //   A triggered cast neither reads nor writes the triggered
                //   ability's `AbilityCooldowns` entry.
                //
                // It is as if, for that one cast, the ability had no cooldown at
                // all. Writing the entry would grief the player (an automatic
                // escape at 25 % HP would put his own manual escape on cooldown
                // he never asked for); *clearing* it would be an outright
                // exploit (fire the trigger, then manually recast the same spell
                // for free). Both systems leave the other's state alone.
                let bypassing_cooldown = cooldown_bypassed(data, token, &spec_ability_copy);
                if !bypassing_cooldown
                    && let Some(cooldown_secs) = ability_meta.cooldown
                    && let Some(id) = spec_ability_copy.ability_id(
                        Some(data.character),
                        data.inventory,
                        data.ability_pool,
                    )
                {
                    output_events.emit_server(SetAbilityCooldownEvent {
                        entity: data.entity,
                        ability_id: id.to_string(),
                        cooldown_secs,
                    });
                }

                // The token was spent: tell the character-behavior system to
                // move this slot from `Firing` to `CoolingDown`. Reached only
                // on the success path, so a refused cast costs nothing.
                if let InputKind::TriggerAbility(slot) = input {
                    update.triggered_slot_cast = Some(slot);
                }

                // Hemomancy "blood price" (M4 / ENG-C1): casting spends the
                // caster's own HP. The activation filter already enforced the
                // 1-HP floor (`hp_cost_affordable`), so in normal play this never
                // brings the caster below 1.
                if let Some(hp_cost) = ability_meta.hp_cost
                    && hp_cost > 0.0
                {
                    output_events.emit_server(HealthChangeEvent {
                        entity: data.entity,
                        change: HealthChange {
                            amount: -hp_cost,
                            by: None,
                            cause: None,
                            magic_source: None,
                            time: *data.time,
                            precise: false,
                            instance: rand::random(),
                        },
                    });
                }

                for init_event in ability
                    .ability_meta()
                    .init_event
                    .iter()
                    .chain(ability.ability_meta().init_event2.iter())
                {
                    match init_event {
                        AbilityInitEvent::EnterStance(stance) => {
                            output_events.emit_server(ChangeStanceEvent {
                                entity: data.entity,
                                stance: *stance,
                            });
                        },
                        AbilityInitEvent::GainBuff {
                            kind,
                            strength,
                            duration,
                        } => {
                            let dest_info = DestInfo {
                                stats: Some(data.stats),
                                mass: Some(data.mass),
                            };
                            output_events.emit_server(BuffEvent {
                                entity: data.entity,
                                buff_change: BuffChange::Add(Buff::new(
                                    *kind,
                                    BuffData::new(*strength, *duration),
                                    vec![BuffCategory::SelfBuff],
                                    BuffSource::Character {
                                        by: *data.uid,
                                        tool_kind,
                                    },
                                    *data.time,
                                    dest_info,
                                    Some(data.mass),
                                    target_uid,
                                    ability_meta.source,
                                )),
                            });
                        },
                        AbilityInitEvent::RemoveBuff(buff) => {
                            output_events.emit_server(BuffEvent {
                                entity: data.entity,
                                buff_change: BuffChange::RemoveByKind(*buff),
                            });
                        },
                    }
                }
                if let CharacterState::Roll(roll) = &mut update.character {
                    if data.character.is_wield() || data.character.was_wielded() {
                        roll.was_wielded = true;
                    }
                    if data.character.is_stealthy() {
                        roll.is_sneaking = true;
                    }
                    if data.character.is_aimed() {
                        roll.prev_aimed_dir = Some(data.controller.inputs.look_dir);
                    }
                }
                return true;
            },
            Err(err) => {
                warn!("Failed to enter character state: {err:?}");
            },
        }
    }
    false
}

/// Whether this cast may skip — and must also refrain from writing —
/// `AbilityCooldowns`.
///
/// 🔴 All three conditions must hold, and the third is load-bearing rather than
/// belt-and-braces:
///
/// 1. the input is `InputKind::TriggerAbility(i)` (otherwise `token` is
///    `None`);
/// 2. slot `i` currently holds an authorisation token, i.e. it is `Firing`;
/// 3. the ability actually resolved has **exactly** the id the token names.
///
/// Without (3), a player pressing Primary on the same tick a trigger armed
/// would win the input `BTreeMap` (lowest discriminant wins), consume the
/// outstanding bypass and get a free cooldown-ignoring Primary. With it, the
/// token is unusable by any input other than the exact slot and the exact
/// ability it was minted for.
fn cooldown_bypassed(
    data: &JoinData<'_>,
    token: Option<&str>,
    spec_ability: &SpecifiedAbility,
) -> bool {
    token.is_some_and(|token| {
        (*spec_ability)
            .ability_id(Some(data.character), data.inventory, data.ability_pool)
            .is_some_and(|id| id == token)
    })
}

/// An ability with `meta.cooldown` may only fire when `AbilityCooldowns` says
/// it is ready. Runs on client and server; the server-side check is
/// authoritative (the event above is server-only), the client check uses the
/// synced component for prediction.
fn cooldown_ready(
    data: &JoinData<'_>,
    ability: &CharacterAbility,
    spec_ability: &SpecifiedAbility,
) -> bool {
    ability.ability_meta().cooldown.is_none_or(|_| {
        (*spec_ability)
            .ability_id(Some(data.character), data.inventory, data.ability_pool)
            .is_none_or(|id| {
                data.ability_cooldowns
                    .is_none_or(|cds| cds.is_ready(id, *data.time))
            })
    })
}

/// Whether an ability's optional HP cost (the Hemomancy "blood price", M4 /
/// ENG-C1) can be paid right now.
/// - **Normal** play keeps a **1-HP floor**: the caster needs `cost + 1`
///   current HP, so a normal cast never self-kills.
/// - **Hardcore** (permadeath) has **no floor** (ENG-C1b, Matias §6.4): the
///   blood price may be lethal, so the cast is always allowed to be paid.
///
/// Entities without a `Health` component (e.g. invulnerable) ignore the cost.
fn hp_cost_affordable(hp_cost: Option<f32>, current_hp: Option<f32>, hardcore: bool) -> bool {
    hp_cost.is_none_or(|cost| hardcore || current_hp.is_none_or(|hp| hp >= cost + 1.0))
}

/// The possession gate for spellcasting: **for a Mage** (spec
/// `2026-08-01-nh26-mage-mastery-percent-design.md` §1 gate 2 — the
/// fragility payoff for the Mage's versatility, not a general caster rule),
/// a spell of `SpellDef::level >= 1` may only be cast while a
/// `ToolKind::Tome` is equipped in either active hand. Cantrips
/// (`level == 0`) are exempt, and so is any `ability_id` that resolves to no
/// compendium entry at all -- bespoke/legacy content that this gate does not
/// govern; its own gating, if any, is authored elsewhere. Any caster who
/// does not hold the Mage class (Sorcerer, Warlock, Cleric, Druid, an
/// entity with no `CharacterClass` at all, ...) is exempt entirely --
/// Sorcerer/Warlock cast without any implement by design, and
/// Cleric/Druid's own implements (Sceptre/HolySymbol/Focus) are not Tomes.
///
/// `ability_id` is `None` for an activation that never resolved to any
/// concrete ability id (e.g. a missing item-config ability set); that case
/// passes for the same reason an uncatalogued id does.
fn tome_possession_ok(
    inventory: Option<&Inventory>,
    character_class: Option<&CharacterClass>,
    ability_id: Option<&str>,
) -> bool {
    if !character_class.is_some_and(|class| class.has(ClassKind::Mage)) {
        return true;
    }
    let Some(ability_id) = ability_id else {
        return true;
    };
    let level = match spell_compendium_manifest().resolve(ability_id) {
        Some(spell) => spell.level,
        // Uncatalogued: not governed by this gate.
        None => return true,
    };
    if level == 0 {
        // Cantrip: no Tome required.
        return true;
    }
    let has_tome = |slot: EquipSlot| {
        matches!(
            inventory
                .and_then(|inv| inv.equipped(slot))
                .map(|item| item.kind())
                .as_deref(),
            Some(ItemKind::Tool(tool)) if tool.kind == ToolKind::Tome
        )
    };
    has_tome(EquipSlot::ActiveMainhand) || has_tome(EquipSlot::ActiveOffhand)
}

pub fn handle_input(
    data: &JoinData<'_>,
    output_events: &mut OutputEvents,
    update: &mut StateUpdate,
    input: InputKind,
) {
    match input {
        InputKind::Primary
        | InputKind::Secondary
        | InputKind::Ability(_)
        | InputKind::Block
        | InputKind::Roll
        | InputKind::TriggerAbility(_) => {
            handle_ability(data, update, output_events, input);
        },
        InputKind::Jump => {
            handle_jump(data, output_events, update, 1.0);
        },
        InputKind::WallJump | InputKind::Fly => {},
    }
}

// NOTE: Quality of Life hack
//
// Uses glider ability if has any, otherwise fallback
pub fn handle_glider_input_or(
    data: &JoinData<'_>,
    update: &mut StateUpdate,
    output_events: &mut OutputEvents,
    fallback_fn: fn(&JoinData<'_>, &mut StateUpdate),
) {
    if data
        .inventory
        .and_then(|inv| inv.equipped(EquipSlot::Glider))
        .and_then(|glider| glider.item_config())
        .is_none()
    {
        fallback_fn(data, update);
        return;
    };

    if let Some(input) = data.controller.queued_inputs.keys().next() {
        handle_ability(data, update, output_events, *input);
    };
}

pub fn attempt_input(
    data: &JoinData<'_>,
    output_events: &mut OutputEvents,
    update: &mut StateUpdate,
) {
    // TODO: look into using first() when it becomes stable
    if let Some(input) = data.controller.queued_inputs.keys().next() {
        handle_input(data, output_events, update, *input);
    }
}

/// Returns whether an interrupt occurred
pub fn handle_interrupts(
    data: &JoinData,
    update: &mut StateUpdate,
    output_events: &mut OutputEvents,
) -> bool {
    let can_dodge = matches!(
        data.character.stage_section(),
        Some(StageSection::Buildup | StageSection::Recover)
    );
    let can_block = data
        .character
        .ability_info()
        .map(|info| info.ability_meta)
        .is_some_and(|meta| meta.capabilities.contains(Capability::BLOCK_INTERRUPT));
    if can_dodge && input_is_pressed(data, InputKind::Roll) {
        handle_ability(data, update, output_events, InputKind::Roll)
    } else if can_block && input_is_pressed(data, InputKind::Block) {
        handle_ability(data, update, output_events, InputKind::Block)
    } else {
        false
    }
}

pub fn is_strafing(data: &JoinData<'_>, update: &StateUpdate) -> bool {
    // TODO: Don't always check `character.is_aimed()`, allow the frontend to
    // control whether the player strafes during an aimed `CharacterState`.
    (update.character.is_aimed() || update.should_strafe) && data.body.can_strafe()
    // no strafe with music instruments equipped in ActiveMainhand
    && !matches!(unwrap_tool_data(data, EquipSlot::ActiveMainhand),
        Some((ToolKind::Instrument, _, _)))
}

/// Returns tool kind, grip and [`WeaponRole`].
pub fn unwrap_tool_data(
    data: &JoinData,
    equip_slot: EquipSlot,
) -> Option<(ToolKind, Hands, WeaponRole)> {
    if let Some(ItemKind::Tool(tool)) = data
        .inventory
        .and_then(|inv| inv.equipped(equip_slot))
        .map(|i| i.kind())
        .as_deref()
    {
        Some((tool.kind, tool.hands, tool.role()))
    } else {
        None
    }
}

pub fn get_hands(data: &JoinData<'_>) -> (Option<Hands>, Option<Hands>) {
    let hand = |slot| {
        if let Some(ItemKind::Tool(tool)) = data
            .inventory
            .and_then(|inv| inv.equipped(slot))
            .map(|i| i.kind())
            .as_deref()
        {
            Some(tool.hands)
        } else {
            None
        }
    };
    (
        hand(EquipSlot::ActiveMainhand),
        hand(EquipSlot::ActiveOffhand),
    )
}

pub fn get_tool_stats(data: &JoinData<'_>, ai: AbilityInfo) -> tool::Stats {
    ai.hand
        .map(|hand| hand.to_equip_slot())
        .and_then(|slot| data.inventory.and_then(|inv| inv.equipped(slot)))
        .and_then(|item| {
            if let ItemKind::Tool(tool) = &*item.kind() {
                Some(tool.stats(item.stats_durability_multiplier()))
            } else {
                None
            }
        })
        .unwrap_or(tool::Stats::one())
}

pub fn input_is_pressed(data: &JoinData<'_>, input: InputKind) -> bool {
    data.controller.queued_inputs.contains_key(&input)
}

/// Checked `Duration` addition. Computes `timer` + `dt`, only applying
/// the explicitly given modifier and returning None if overflow
/// occurred.
fn checked_tick(data: &JoinData<'_>, timer: Duration, modifier: Option<f32>) -> Option<Duration> {
    timer.checked_add(Duration::from_secs_f32(data.dt.0 * modifier.unwrap_or(1.0)))
}

/// Ticks `timer` by `dt`, only applying the explicitly given modifier.
/// Returns `Duration::default()` if overflow occurs
pub fn tick_or_default(data: &JoinData<'_>, timer: Duration, modifier: Option<f32>) -> Duration {
    checked_tick(data, timer, modifier).unwrap_or_default()
}

/// Checked `Duration` addition. Computes `timer` + `dt`, applying relevant stat
/// attack modifiers and returning None if overflow
/// occurred.
fn checked_tick_attack(
    data: &JoinData<'_>,
    timer: Duration,
    other_modifier: Option<f32>,
) -> Option<Duration> {
    let section_modifier = match data.character.stage_section() {
        Some(StageSection::Buildup) => data.stats.buildup_speed_modifier,
        Some(StageSection::Charge) => data.stats.charge_speed_modifier,
        Some(StageSection::Recover) => data.stats.recovery_speed_modifier,
        _ => 1.0,
    };
    checked_tick(
        data,
        timer,
        Some(data.stats.attack_speed_modifier * section_modifier * other_modifier.unwrap_or(1.0)),
    )
}

/// Ticks `timer` by `dt`, applying relevant stat attack modifiers and
/// `other_modifier`. Returns `Duration::default()` if overflow occurs
pub fn tick_attack_or_default(
    data: &JoinData<'_>,
    timer: Duration,
    other_modifier: Option<f32>,
) -> Duration {
    checked_tick_attack(data, timer, other_modifier).unwrap_or_default()
}

/// Determines what portion a state is in. Used in all attacks (eventually). Is
/// used to control aspects of animation code, as well as logic within the
/// character states.
#[derive(Clone, Copy, Debug, Display, Eq, Hash, PartialEq, Serialize, Deserialize)]
pub enum StageSection {
    Buildup,
    Recover,
    Charge,
    Movement,
    Action,
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub enum ForcedMovement {
    Forward(f32),
    Reverse(f32),
    Sideways(f32),
    DirectedReverse(f32),
    AntiDirectedForward(f32),
    Leap {
        vertical: f32,
        forward: f32,
        progress: f32,
        direction: MovementDirection,
    },
}

impl Mul<f32> for ForcedMovement {
    type Output = Self;

    fn mul(self, scalar: f32) -> Self {
        use ForcedMovement::*;
        match self {
            Forward(x) => Forward(x * scalar),
            Reverse(x) => Reverse(x * scalar),
            Sideways(x) => Sideways(x * scalar),
            DirectedReverse(x) => DirectedReverse(x * scalar),
            AntiDirectedForward(x) => AntiDirectedForward(x * scalar),
            Leap {
                vertical,
                forward,
                progress,
                direction,
            } => Leap {
                vertical: vertical * scalar,
                forward: forward * scalar,
                progress,
                direction,
            },
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum MovementDirection {
    Look,
    AntiLook,
    Move,
}

impl MovementDirection {
    pub fn get_2d_dir(self, data: &JoinData<'_>) -> Vec2<f32> {
        use MovementDirection::*;
        match self {
            Look => data
                .inputs
                .look_dir
                .to_horizontal()
                .unwrap_or_default()
                .xy(),
            AntiLook => -data
                .inputs
                .look_dir
                .to_horizontal()
                .unwrap_or_default()
                .xy(),
            Move => data.inputs.move_dir,
        }
        .try_normalized()
        .unwrap_or_default()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct AbilityInfo {
    pub tool: Option<ToolKind>,
    pub hand: Option<HandInfo>,
    /// The equipped tool's [`WeaponRole`], mirroring `hand`: `None` when
    /// `tool` is `None` (unarmed/natural/`Empty`-tool attacks), `Some`
    /// otherwise. Threaded alongside `hand` so `Attack::proficiency_multiplier`
    /// can narrow by role exactly as it already narrows by grip.
    pub role: Option<WeaponRole>,
    pub input: InputKind,
    pub input_attr: Option<InputAttr>,
    pub ability_meta: AbilityMeta,
    pub ability: Option<SpecifiedAbility>,
}

impl AbilityInfo {
    pub fn new(
        data: &JoinData<'_>,
        from_offhand: bool,
        input: InputKind,
        ability: Option<SpecifiedAbility>,
        ability_meta: AbilityMeta,
    ) -> Self {
        let tool_data = if from_offhand {
            unwrap_tool_data(data, EquipSlot::ActiveOffhand)
        } else {
            unwrap_tool_data(data, EquipSlot::ActiveMainhand)
        };
        let (tool, hand, role) = tool_data.map_or((None, None, None), |(kind, hands, role)| {
            (
                Some(kind),
                Some(HandInfo::from_main_tool(hands, from_offhand)),
                Some(role),
            )
        });

        Self {
            tool,
            hand,
            role,
            input,
            input_attr: data.controller.queued_inputs.get(&input).copied(),
            ability_meta,
            ability,
        }
    }
}

pub fn end_ability(data: &JoinData<'_>, update: &mut StateUpdate) {
    if data.character.is_wield() || data.character.was_wielded() {
        update.character = CharacterState::Wielding(wielding::Data {
            is_sneaking: data.character.is_stealthy(),
        });
    } else {
        update.character = CharacterState::Idle(idle::Data {
            is_sneaking: data.character.is_stealthy(),
            footwear: None,
            time_entered: *data.time,
        });
    }
    if let CharacterState::Roll(roll) = data.character
        && let Some(dir) = roll.prev_aimed_dir
    {
        update.ori = dir.into();
    }
}

pub fn end_melee_ability(data: &JoinData<'_>, update: &mut StateUpdate) {
    end_ability(data, update);
    data.updater.remove::<Melee>(data.entity);
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum HandInfo {
    TwoHanded,
    MainHand,
    OffHand,
}

impl HandInfo {
    pub fn from_main_tool(tool_hands: Hands, from_offhand: bool) -> Self {
        match tool_hands {
            Hands::Two => Self::TwoHanded,
            Hands::One => {
                if from_offhand {
                    Self::OffHand
                } else {
                    Self::MainHand
                }
            },
        }
    }

    pub fn to_equip_slot(&self) -> EquipSlot {
        match self {
            HandInfo::TwoHanded | HandInfo::MainHand => EquipSlot::ActiveMainhand,
            HandInfo::OffHand => EquipSlot::ActiveOffhand,
        }
    }
}

pub fn leave_stance(data: &JoinData<'_>, output_events: &mut OutputEvents) {
    if !matches!(data.stance, Some(Stance::None)) {
        output_events.emit_server(ChangeStanceEvent {
            entity: data.entity,
            stance: Stance::None,
        });
    }
}

#[derive(Copy, Clone, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum ComboConsumption {
    #[default]
    All,
    Half,
    Cost,
}

impl ComboConsumption {
    pub fn consume(&self, data: &JoinData, output_events: &mut OutputEvents, cost: u32) {
        let combo = data.combo.map_or(0, |c| c.counter());
        let to_consume = match self {
            Self::All => combo,
            Self::Half => combo.div_ceil(2),
            Self::Cost => cost,
        };
        output_events.emit_server(ComboChangeEvent {
            entity: data.entity,
            change: -(to_consume as i32),
        });
    }
}

fn loadout_change_hook(data: &JoinData<'_>, output_events: &mut OutputEvents, clear_combo: bool) {
    if clear_combo {
        // Reset combo to 0
        output_events.emit_server(ComboChangeEvent {
            entity: data.entity,
            change: -data.combo.map_or(0, |c| c.counter() as i32),
        });
    }
    // Clear any buffs from equipped weapons
    output_events.emit_server(BuffEvent {
        entity: data.entity,
        buff_change: BuffChange::RemoveByCategory {
            all_required: vec![BuffCategory::RemoveOnLoadoutChange],
            any_required: vec![],
            none_required: vec![],
        },
    });
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct MovementModifier {
    pub buildup: Option<f32>,
    pub action: Option<f32>,
    pub recover: Option<f32>,
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct OrientationModifier {
    pub buildup: Option<f32>,
    pub action: Option<f32>,
    pub recover: Option<f32>,
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub enum ProjectileSpread {
    Increasing(f32),
    Horizontal(f32),
}

impl ProjectileSpread {
    pub fn compute_directions(
        self,
        init_dir: Dir,
        init_ori: Ori,
        num: u32,
        rng: &mut impl RngExt,
    ) -> impl Iterator<Item = Dir> + '_ {
        match self {
            Self::Increasing(spread) => Either::Left(
                // Adds a slight spread to the projectiles. First projectile has no spread,
                // and spread increases linearly with number of projectiles created.
                (0..num).map(move |i| {
                    Dir::from_unnormalized(init_dir.map(|x| {
                        let offset = (2.0 * rng.random::<f32>() - 1.0) * spread * i as f32;
                        x + offset
                    }))
                    .unwrap_or(init_dir)
                }),
            ),
            Self::Horizontal(spread) => Either::Right(if num < 2 {
                Either::Left(std::iter::once(init_dir))
            } else {
                let left = -spread.to_radians();
                let increment = spread.to_radians() * 2.0 / (num as f32 - 1.0);
                let rot_quat_dir = Quaternion::<f32>::rotation_from_to_3d(
                    Vec3::unit_y(),
                    Vec3::new(0.0, init_dir.xy().magnitude(), init_dir.z),
                );
                Either::Right((0..num).map(move |i| {
                    let angle = left + increment * i as f32;
                    let rot_quat_spread = Quaternion::<f32>::rotation_from_to_3d(
                        Vec3::unit_y(),
                        Vec2::unit_y().rotated_z(angle).with_z(0.0),
                    );
                    Dir::from_unnormalized(
                        Ori::new(init_ori.to_quat() * rot_quat_dir * rot_quat_spread).look_vec(),
                    )
                    .unwrap_or(init_dir)
                }))
            }),
        }
    }

    /// Don't use this for anything important, just things that need to know
    /// "roughly" the spread
    pub fn estimated_spread(&self) -> f32 {
        match self {
            // TODO: Check if we want these to return something different
            Self::Increasing(spread) | Self::Horizontal(spread) => *spread,
        }
    }
}

#[cfg(test)]
mod hp_cost_tests {
    use super::hp_cost_affordable;

    // M4 (ENG-C1/C1b): the Hemomancy "blood price". Normal mode keeps a 1-HP
    // floor — you need `cost + 1` current HP to cast, so a normal cast never
    // self-kills. Hardcore has no floor (the price may be lethal).
    #[test]
    fn none_cost_is_free() {
        assert!(hp_cost_affordable(None, Some(0.5), false));
        assert!(hp_cost_affordable(None, None, false));
        assert!(hp_cost_affordable(None, Some(0.5), true));
    }

    #[test]
    fn normal_requires_cost_plus_one_floor() {
        assert!(hp_cost_affordable(Some(10.0), Some(11.0), false)); // exactly cost+1 → leaves 1 HP
        assert!(hp_cost_affordable(Some(10.0), Some(50.0), false)); // plenty
        assert!(!hp_cost_affordable(Some(10.0), Some(10.5), false)); // below the 1-HP floor
        assert!(!hp_cost_affordable(Some(10.0), Some(10.0), false)); // can't pay full and keep the floor
        assert!(!hp_cost_affordable(Some(10.0), Some(3.0), false)); // nowhere near
    }

    #[test]
    fn hardcore_has_no_floor() {
        // ENG-C1b: hardcore can pay the blood price even when it would be lethal
        assert!(hp_cost_affordable(Some(10.0), Some(10.0), true));
        assert!(hp_cost_affordable(Some(10.0), Some(3.0), true));
        assert!(hp_cost_affordable(Some(10.0), Some(0.5), true));
    }

    #[test]
    fn missing_health_skips_cost() {
        // entities without a Health component (e.g. invulnerable) ignore the cost
        assert!(hp_cost_affordable(Some(10.0), None, false));
    }
}

#[cfg(test)]
mod possession_gate_tests {
    use super::tome_possession_ok;
    use crate::{
        comp::{
            Inventory, Item,
            class::{CharacterClass, ClassKind},
            inventory::slot::EquipSlot,
        },
        resources::Time,
    };

    // Real compendium entries, chosen to cover a cantrip, a low spell level
    // and the highest normal spell level.
    const CANTRIP: &str = "spells.hemomancy.bloodlet"; // level 0
    const LEVEL_ONE: &str = "spells.hemomancy.hemal_spike"; // level 1
    const LEVEL_NINE: &str = "spells.hemomancy.the_last_vein"; // level 9
    const UNCATALOGUED: &str = "spells.not_a_real_spell.made_up_for_this_test";

    fn mage() -> CharacterClass { CharacterClass::single(ClassKind::Mage) }

    fn inventory_with_tome_in(slot: EquipSlot) -> Inventory {
        let mut inv = Inventory::with_empty();
        inv.replace_loadout_item(
            slot,
            Some(Item::new_from_asset_expect(
                "common.items.weapons.tome.apprentice_tome",
            )),
            Time(0.0),
        );
        inv
    }

    #[test]
    fn cantrip_with_no_tome_is_allowed() {
        assert!(tome_possession_ok(None, Some(&mage()), Some(CANTRIP)));
        assert!(tome_possession_ok(
            Some(&Inventory::with_empty()),
            Some(&mage()),
            Some(CANTRIP)
        ));
    }

    #[test]
    fn levelled_spell_with_no_tome_is_refused_for_a_mage() {
        assert!(!tome_possession_ok(None, Some(&mage()), Some(LEVEL_ONE)));
        assert!(!tome_possession_ok(
            Some(&Inventory::with_empty()),
            Some(&mage()),
            Some(LEVEL_ONE)
        ));
    }

    #[test]
    fn levelled_spell_with_tome_in_mainhand_is_allowed() {
        let inv = inventory_with_tome_in(EquipSlot::ActiveMainhand);
        assert!(tome_possession_ok(
            Some(&inv),
            Some(&mage()),
            Some(LEVEL_ONE)
        ));
    }

    #[test]
    fn levelled_spell_with_tome_in_offhand_is_allowed() {
        let inv = inventory_with_tome_in(EquipSlot::ActiveOffhand);
        assert!(tome_possession_ok(
            Some(&inv),
            Some(&mage()),
            Some(LEVEL_ONE)
        ));
    }

    #[test]
    fn highest_spell_level_with_tome_is_allowed() {
        let inv = inventory_with_tome_in(EquipSlot::ActiveMainhand);
        assert!(tome_possession_ok(
            Some(&inv),
            Some(&mage()),
            Some(LEVEL_NINE)
        ));
    }

    #[test]
    fn uncatalogued_ability_with_no_tome_is_allowed() {
        // Bespoke/legacy content with no compendium entry is not governed by
        // this gate at all.
        assert!(tome_possession_ok(None, Some(&mage()), Some(UNCATALOGUED)));
        assert!(tome_possession_ok(
            Some(&Inventory::with_empty()),
            Some(&mage()),
            Some(UNCATALOGUED)
        ));
    }

    #[test]
    fn missing_ability_id_is_allowed() {
        assert!(tome_possession_ok(None, Some(&mage()), None));
        assert!(tome_possession_ok(
            Some(&Inventory::with_empty()),
            Some(&mage()),
            None
        ));
    }

    /// The bug this test guards against: the gate must never apply to a
    /// caster who isn't a Mage. Sorcerer/Warlock cast without any implement
    /// by design; Cleric/Druid's own implements (Sceptre/HolySymbol/Focus)
    /// are not Tomes and were never meant to be gated by this rule.
    #[test]
    fn levelled_spell_with_no_tome_is_allowed_for_a_non_mage_caster() {
        for class in [
            ClassKind::Sorcerer,
            ClassKind::Warlock,
            ClassKind::Cleric,
            ClassKind::Druid,
        ] {
            let character_class = CharacterClass::single(class);
            assert!(
                tome_possession_ok(None, Some(&character_class), Some(LEVEL_ONE)),
                "{class:?} should not be gated by the Mage-specific Tome possession rule"
            );
        }
    }

    #[test]
    fn levelled_spell_with_no_tome_is_allowed_with_no_character_class() {
        // Entities with no CharacterClass at all (e.g. NPCs) are exempt,
        // matching SpellCompendium::allows's own None-is-permissive rule.
        assert!(tome_possession_ok(None, None, Some(LEVEL_ONE)));
    }

    #[test]
    fn a_multiclass_mage_is_still_gated() {
        let multiclass = CharacterClass {
            primary: ClassKind::Warlock,
            secondary: Some(ClassKind::Mage),
            secondary_level: 5,
            future_levels_to_secondary: false,
        };
        assert!(!tome_possession_ok(
            None,
            Some(&multiclass),
            Some(LEVEL_ONE)
        ));
    }
}

#[cfg(test)]
mod cast_gate_asset_tests {
    use crate::comp::{
        class::{CharacterClass, ClassKind, class_proficiencies_manifest},
        inventory::item::tool::{AbilityKind, AbilityMap, AbilitySpec, ToolKind},
        spell::spell_compendium_manifest,
    };

    /// Every class proficient with `tool` (able to equip it), derived from
    /// `class_proficiencies.ron` rather than hardcoded, so this test tracks
    /// the manifest instead of needing a manual update whenever a class's
    /// weapon list changes.
    fn classes_proficient_with(tool: ToolKind) -> Vec<ClassKind> {
        let manifest = class_proficiencies_manifest();
        ClassKind::ALL
            .into_iter()
            .filter(|&class| {
                CharacterClass::single(class)
                    .proficient_tools_mask(&manifest.0)
                    // `role: None` -- permissive across both roles, since
                    // this helper asks "proficient with `tool` at all"
                    // (either its caster or martial kit), not one role
                    // specifically.
                    .allows(tool, None, None)
            })
            .collect()
    }

    /// Every ability an implement's `primary`/`secondary`/`abilities`
    /// resolves to (in `ability_set_manifest.ron`) must clear the per-spell
    /// class filter for at least one class that can actually equip that
    /// implement. A class-gated implement whose own attack no class holding
    /// it can cast is unusable content; this test exists to keep that
    /// caught.
    ///
    /// Includes `Staff`/`Sceptre`: their `primary`/`secondary`/`abilities`
    /// resolve to the legacy fire/warding kit, which is wired directly into
    /// these `Tool(ToolKind)` ability sets rather than the pool, so this is
    /// the same class-filter check `Tome`/`HolySymbol`/`Focus` already get,
    /// just reached through a different `ability_id` keying (see
    /// `SpellCompendium::allows`'s `by_ability` doc comment).
    #[test]
    fn implement_abilities_are_castable_by_a_class_that_can_equip_them() {
        let ability_map = AbilityMap::load().read();
        let compendium = spell_compendium_manifest();

        for tool in [
            ToolKind::Tome,
            ToolKind::HolySymbol,
            ToolKind::Focus,
            ToolKind::Staff,
            ToolKind::Sceptre,
        ] {
            let proficient_classes = classes_proficient_with(tool);
            assert!(
                !proficient_classes.is_empty(),
                "no class is proficient with {tool:?} — check class_proficiencies.ron"
            );

            let ability_set = ability_map
                .get_ability_set(&AbilitySpec::Tool(tool))
                .unwrap_or_else(|| {
                    panic!("no ability set for {tool:?} in ability_set_manifest.ron")
                });

            let mut slots: Vec<(String, &AbilityKind<_>)> = vec![
                ("primary".to_string(), &ability_set.primary),
                ("secondary".to_string(), &ability_set.secondary),
            ];
            slots.extend(
                ability_set
                    .abilities
                    .iter()
                    .enumerate()
                    .map(|(i, kind)| (format!("abilities[{i}]"), kind)),
            );

            for (slot, kind) in slots {
                let AbilityKind::Simple(_, item) = kind else {
                    panic!("{tool:?} {slot} is Contextualized; extend this test to walk it");
                };
                let castable_by_some_class = proficient_classes.iter().any(|&class| {
                    compendium.allows(&item.id, Some(&CharacterClass::single(class)))
                });

                assert!(
                    castable_by_some_class,
                    "{tool:?} {slot} ability {:?} is not castable by any class proficient with \
                     {tool:?} ({proficient_classes:?})",
                    item.id,
                );
            }
        }
    }

    /// The actual gap this catalogue closes, asserted from the exact
    /// activation-independent angle: a class NOT proficient with `Staff`/
    /// `Sceptre` at all (so it could never satisfy an equip gate on any
    /// ItemDef, standard or modular) must be refused by every ability in
    /// that tool's `Tool(ToolKind)` set. `Item::requirements()` now unions a
    /// per-item `requirements:` block with the `equip_gates.ron` manifest for
    /// modular weapons too, so a modular Staff/Sceptre also gets a real equip
    /// gate -- but this check, driven purely by `SpellCompendium::allows`
    /// (which never consults item requirements), is defense in depth: even a
    /// hypothetical future item shape with no equip gate at all would still
    /// be caught here.
    #[test]
    fn legacy_kit_refuses_a_class_with_no_staff_sceptre_proficiency_at_all() {
        let ability_map = AbilityMap::load().read();
        let compendium = spell_compendium_manifest();

        for tool in [ToolKind::Staff, ToolKind::Sceptre] {
            let proficient = classes_proficient_with(tool);
            let outsider = ClassKind::ALL
                .into_iter()
                .find(|class| !proficient.contains(class))
                .unwrap_or_else(|| panic!("every class is proficient with {tool:?}?"));

            let ability_set = ability_map
                .get_ability_set(&AbilitySpec::Tool(tool))
                .unwrap_or_else(|| {
                    panic!("no ability set for {tool:?} in ability_set_manifest.ron")
                });

            let mut kinds: Vec<&AbilityKind<_>> =
                vec![&ability_set.primary, &ability_set.secondary];
            kinds.extend(ability_set.abilities.iter());

            for kind in kinds {
                let AbilityKind::Simple(_, item) = kind else {
                    continue;
                };
                assert!(
                    !compendium.allows(&item.id, Some(&CharacterClass::single(outsider))),
                    "{outsider:?} (not proficient with {tool:?}) can still cast {:?} — the \
                     modular-weapon gap is not closed",
                    item.id,
                );
            }
        }
    }
}
