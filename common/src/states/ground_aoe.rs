use crate::{
    Damage, DamageKind, Explosion, GroupTarget, Knockback, RadiusEffect,
    combat::{Attack, AttackDamage, AttackEffect, CombatEffect, CombatRequirement, PooledDebuff},
    comp::{
        CharacterState, StateUpdate, ability::Dodgeable, character_state::OutputEvents,
        item::Reagent,
    },
    event::ExplosionEvent,
    states::{
        behavior::{CharacterBehavior, JoinData},
        utils::*,
    },
    terrain::Block,
    vol::ReadVol,
};
use serde::{Deserialize, Serialize};
use std::time::Duration;
use vek::Vec3;

#[derive(Copy, Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct StaticData {
    pub buildup_duration: Duration,
    /// Telegraph duration between target lock and strike
    pub delay: Duration,
    pub recover_duration: Duration,
    pub max_range: f32,
    pub radius: f32,
    pub min_falloff: f32,
    pub damage: f32,
    pub poise: f32,
    pub knockback: Knockback,
    pub dodgeable: Dodgeable,
    pub reagent: Option<Reagent>,
    pub rooted_cast: bool,
    /// See `CharacterAbility::GroundAoe::pooled_debuff` -- when set, the
    /// strike below emits only this pooled debuff and skips the normal
    /// `damage`/`poise`/`knockback` attack entirely.
    pub pooled_debuff: Option<PooledDebuff>,
    pub ability_info: AbilityInfo,
}

#[derive(Copy, Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Data {
    pub static_data: StaticData,
    pub timer: Duration,
    pub stage_section: StageSection,
    /// Locked at the Buildup -> Action transition; the telegraph and strike
    /// happen here even if the caster moves away.
    pub target_pos: Option<Vec3<f32>>,
}

impl CharacterBehavior for Data {
    fn behavior(&self, data: &JoinData, output_events: &mut OutputEvents) -> StateUpdate {
        let mut update = StateUpdate::from(data);

        handle_orientation(data, &mut update, 1.0, None);
        let move_efficiency = if self.static_data.rooted_cast {
            0.0
        } else {
            0.7
        };
        handle_move(data, &mut update, move_efficiency);

        match self.stage_section {
            StageSection::Buildup => {
                if self.timer < self.static_data.buildup_duration {
                    update.character = CharacterState::GroundAoe(Data {
                        timer: tick_attack_or_default(data, self.timer, None),
                        ..*self
                    });
                } else {
                    // Lock the target: client-selected ground pos (same trust
                    // model as Blink), clamped to max_range; fallback to a
                    // point max_range ahead along the look dir.
                    let aim = self
                        .static_data
                        .ability_info
                        .input_attr
                        .and_then(|attr| attr.select_pos)
                        .unwrap_or_else(|| {
                            data.pos.0 + *data.inputs.look_dir * self.static_data.max_range
                        });
                    let offset = aim - data.pos.0;
                    let clamped = if offset.magnitude_squared() > self.static_data.max_range.powi(2)
                    {
                        data.pos.0 + offset.normalized() * self.static_data.max_range
                    } else {
                        aim
                    };
                    // LoS guard (review M2): pull the strike point back to the
                    // first terrain hit so casts can't land through walls.
                    let eye = data.pos.0
                        + Vec3::unit_z() * data.body.eye_height(data.scale.map_or(1.0, |s| s.0));
                    let ray = data.terrain.ray(eye, clamped).until(Block::is_solid).cast();
                    let target = if ray.0 < (clamped - eye).magnitude() {
                        eye + (clamped - eye).try_normalized().unwrap_or_default() * ray.0
                    } else {
                        clamped
                    };
                    update.character = CharacterState::GroundAoe(Data {
                        timer: Duration::default(),
                        stage_section: StageSection::Action,
                        target_pos: Some(target),
                        ..*self
                    });
                }
            },
            StageSection::Action => {
                if self.timer < self.static_data.delay {
                    update.character = CharacterState::GroundAoe(Data {
                        timer: tick_attack_or_default(data, self.timer, None),
                        ..*self
                    });
                } else {
                    // Strike: server resolves an explosion at the locked pos.
                    //
                    // A pool-driven cast (e.g. `Sleep`) has no direct
                    // damage/poise/knockback of its own -- routing it
                    // through `RadiusEffect::Attack` with a zero damage
                    // instance would still run the full to-hit/dodge/block
                    // pipeline and emit an "attacked" outcome against every
                    // unaffected target, which is both wasted work and
                    // wrong presentation for a spell that isn't attacking
                    // full-health/tough creatures at all. So it emits only
                    // the pooled debuff and skips the attack effect
                    // entirely; every other GroundAoe keeps the original
                    // damage-attack-only path unchanged.
                    if let Some(pos) = self.target_pos {
                        let effects = if let Some(pooled_debuff) = self.static_data.pooled_debuff {
                            vec![RadiusEffect::PooledDebuff(pooled_debuff)]
                        } else {
                            vec![RadiusEffect::Attack {
                                attack: Attack::new(Some(self.static_data.ability_info))
                                    .with_damage(AttackDamage::new(
                                        Damage {
                                            kind: DamageKind::Energy,
                                            value: self.static_data.damage,
                                        },
                                        Some(GroupTarget::OutOfGroup),
                                        rand::random(),
                                    ))
                                    .with_effect(
                                        AttackEffect::new(
                                            Some(GroupTarget::OutOfGroup),
                                            CombatEffect::Poise(self.static_data.poise),
                                        )
                                        .with_requirement(CombatRequirement::AnyDamage),
                                    )
                                    .with_effect(
                                        AttackEffect::new(
                                            Some(GroupTarget::OutOfGroup),
                                            CombatEffect::Knockback(self.static_data.knockback),
                                        )
                                        .with_requirement(CombatRequirement::AnyDamage),
                                    ),
                                dodgeable: self.static_data.dodgeable,
                            }]
                        };
                        output_events.emit_server(ExplosionEvent {
                            pos,
                            explosion: Explosion {
                                effects,
                                radius: self.static_data.radius,
                                reagent: self.static_data.reagent,
                                min_falloff: self.static_data.min_falloff,
                            },
                            owner: Some(*data.uid),
                        });
                    }
                    update.character = CharacterState::GroundAoe(Data {
                        timer: Duration::default(),
                        stage_section: StageSection::Recover,
                        ..*self
                    });
                }
            },
            StageSection::Recover => {
                if self.timer < self.static_data.recover_duration {
                    // Recovery
                    update.character = CharacterState::GroundAoe(Data {
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
                end_ability(data, &mut update);
            },
        }

        // At end of state logic so an interrupt isn't overwritten —
        // poise breaks during Buildup cancel the cast (spec §8 counterplay).
        handle_interrupts(data, &mut update, output_events);

        update
    }
}
