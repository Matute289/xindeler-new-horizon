use serde::{Deserialize, Serialize};
use specs::{Component, FlaggedStorage, VecStorage};
use std::f32::consts::{PI, TAU};
use vek::{Mat3, Vec3};

const TURRET_AIM_EPSILON: f32 = 0.001;

/// The current articulated pose of a citadel cannon in its authored local
/// coordinate space. Yaw is cyclic; pitch is mechanically bounded.
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct CitadelTurretAngles {
    pub yaw: f32,
    pub pitch: f32,
}

impl Component for CitadelTurretAngles {
    type Storage = FlaggedStorage<Self, VecStorage<Self>>;
}

/// Geometric presentation used by a Cromatolis energy field.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum CitadelForceFieldShape {
    Dome,
    /// A hollow semi-bubble extending below its pivot. Used by the first
    /// under-island cannon, whose protected firing volume opens downward.
    InvertedDome,
    /// A server-collidable safety plate with no client mesh. The physical
    /// stone platform remains visible instead of being covered by an opaque
    /// transparent-pass floor.
    SafetyFloor,
    Floor,
}

/// A client-rendered, hollow force field projected by Cromatolis.
///
/// Domes remain visual-only in the first cannon slice. The lower lookout's
/// floor receives a separate server-side collider so players can stand on it;
/// durability, projectile filtering, and automatic defences remain future
/// systems. Keeping the dimensions on the entity lets that later work share
/// the same authored field volume without rebuilding world terrain.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct CitadelForceFieldVisual {
    pub shape: CitadelForceFieldShape,
    /// Horizontal radius in world metres.
    pub horizontal_radius: f32,
    /// Vertical reach from the cannon pivot in world metres. `Dome` extends
    /// upward; `InvertedDome` extends downward.
    pub height: f32,
}

impl CitadelForceFieldVisual {
    /// Every upper cannon tower uses a 28 m wide, 20 m tall semi-bubble.
    pub const fn upper_tower() -> Self {
        Self {
            shape: CitadelForceFieldShape::Dome,
            horizontal_radius: 14.0,
            height: 20.0,
        }
    }

    /// Compatibility name for the first two manually operated pilot stations.
    pub const fn pilot_tower() -> Self { Self::upper_tower() }

    /// An invisible lower-tower safety floor. The server gives this shape a
    /// solid collision plate at its pivot; the visible floor is the authored
    /// stone mounting platform, not a second opaque cyan surface.
    pub const fn lower_platform() -> Self {
        Self {
            shape: CitadelForceFieldShape::SafetyFloor,
            horizontal_radius: 10.0,
            height: 0.0,
        }
    }

    /// Hollow downward-facing bubble for the first lower-tower cannon.
    /// The platform below its pivot is collision-safe separately; the dome
    /// is still visual-only until force-field physics is introduced.
    pub const fn lower_tower_dome() -> Self {
        Self {
            shape: CitadelForceFieldShape::InvertedDome,
            horizontal_radius: 12.0,
            height: 18.0,
        }
    }
}

impl Component for CitadelForceFieldVisual {
    type Storage = FlaggedStorage<Self, VecStorage<Self>>;
}

/// Authored aiming limits for an articulated citadel cannon.
///
/// `mount_inverse` supplied to the aiming functions transforms Veloren's
/// Z-up world space into the cannon model's local Y-up, Z-forward space.
/// This keeps model-axis bindings and balance values out of the generic math.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct CitadelTurretLimits {
    pub pitch_min: f32,
    pub pitch_max: f32,
    pub city_sector_start: Option<f32>,
    pub city_sector_end: Option<f32>,
    pub yaw_speed: f32,
    pub pitch_speed: f32,
}

fn wrap_turret_angle(angle: f32) -> f32 { (angle + PI).rem_euclid(TAU) - PI }

fn turret_ccw_distance(from: f32, to: f32) -> f32 { (to - from).rem_euclid(TAU) }

fn turret_angle_is_in_city_sector(yaw: f32, limits: &CitadelTurretLimits) -> bool {
    let (Some(start), Some(end)) = (limits.city_sector_start, limits.city_sector_end) else {
        return false;
    };

    let sector_width = turret_ccw_distance(start, end);
    sector_width >= TURRET_AIM_EPSILON
        && turret_ccw_distance(start, wrap_turret_angle(yaw)) <= sector_width
}

/// Checks whether the counter-clockwise arc from `from` to `to` crosses the
/// city sector. Callers must reject endpoints inside the sector separately.
fn turret_ccw_arc_crosses_city(from: f32, to: f32, limits: &CitadelTurretLimits) -> bool {
    let (Some(start), Some(end)) = (limits.city_sector_start, limits.city_sector_end) else {
        return false;
    };

    let travel = turret_ccw_distance(from, to);
    let sector_width = turret_ccw_distance(start, end);
    if travel < TURRET_AIM_EPSILON || sector_width < TURRET_AIM_EPSILON {
        return false;
    }

    turret_ccw_distance(from, start) < travel || turret_ccw_distance(from, end) < travel
}

fn move_toward_citadel_turret_angle(current: f32, target: f32, max_step: f32) -> f32 {
    let delta = wrap_turret_angle(target - current);
    if delta.abs() <= max_step {
        wrap_turret_angle(target)
    } else {
        wrap_turret_angle(current + delta.signum() * max_step)
    }
}

/// Moves one yaw step without allowing the path to enter the city sector.
/// If the short arc crosses the city, the clear long arc is selected instead.
fn move_citadel_turret_yaw_safely(
    current: f32,
    target: f32,
    max_step: f32,
    limits: &CitadelTurretLimits,
) -> Option<f32> {
    let current = wrap_turret_angle(current);
    let target = wrap_turret_angle(target);

    if limits.city_sector_start.is_none() || limits.city_sector_end.is_none() {
        return Some(move_toward_citadel_turret_angle(current, target, max_step));
    }

    if turret_angle_is_in_city_sector(current, limits)
        || turret_angle_is_in_city_sector(target, limits)
    {
        return None;
    }

    let ccw_distance = turret_ccw_distance(current, target);
    let cw_distance = turret_ccw_distance(target, current);
    let ccw_blocked = turret_ccw_arc_crosses_city(current, target, limits);
    let cw_blocked = turret_ccw_arc_crosses_city(target, current, limits);

    let (direction, remaining) = match (ccw_blocked, cw_blocked) {
        (false, true) => (1.0, ccw_distance),
        (true, false) => (-1.0, cw_distance),
        (false, false) if ccw_distance <= cw_distance => (1.0, ccw_distance),
        (false, false) => (-1.0, cw_distance),
        (true, true) => return None,
    };

    Some(wrap_turret_angle(
        current + direction * max_step.max(0.0).min(remaining),
    ))
}

fn move_toward_citadel_turret(current: f32, target: f32, max_step: f32) -> f32 {
    let delta = target - current;
    if delta.abs() <= max_step {
        target
    } else {
        current + delta.signum() * max_step.max(0.0)
    }
}

/// Resolves an aim pose for a target without acquiring, tracking, or firing
/// at it. The physical target direction is checked against the city exclusion
/// zone before equivalent yaw/pitch representations are considered.
pub fn desired_citadel_turret_angles(
    pivot_world: Vec3<f32>,
    target_world: Vec3<f32>,
    mount_inverse: Mat3<f32>,
    current: CitadelTurretAngles,
    limits: &CitadelTurretLimits,
) -> Option<CitadelTurretAngles> {
    let local = mount_inverse * (target_world - pivot_world);
    let horizontal = (local.x * local.x + local.z * local.z).sqrt();
    if !horizontal.is_finite() || horizontal < TURRET_AIM_EPSILON {
        return None;
    }

    let target_yaw = wrap_turret_angle(local.x.atan2(local.z));
    if turret_angle_is_in_city_sector(target_yaw, limits) {
        return None;
    }

    let elevation = local.y.atan2(horizontal);
    [
        CitadelTurretAngles {
            yaw: target_yaw,
            pitch: elevation,
        },
        CitadelTurretAngles {
            yaw: wrap_turret_angle(target_yaw + PI),
            pitch: PI - elevation,
        },
    ]
    .into_iter()
    .filter(|candidate| candidate.pitch >= limits.pitch_min && candidate.pitch <= limits.pitch_max)
    .min_by(|left, right| {
        let left_cost =
            wrap_turret_angle(left.yaw - current.yaw).abs() + (left.pitch - current.pitch).abs();
        let right_cost =
            wrap_turret_angle(right.yaw - current.yaw).abs() + (right.pitch - current.pitch).abs();
        left_cost.total_cmp(&right_cost)
    })
}

/// Advances a manually supplied cannon target by one server tick. A rejected
/// target or an impossible safe yaw path leaves the turret untouched.
pub fn update_citadel_turret(
    current: &mut CitadelTurretAngles,
    pivot_world: Vec3<f32>,
    target_world: Vec3<f32>,
    mount_inverse: Mat3<f32>,
    limits: &CitadelTurretLimits,
    dt: f32,
) -> bool {
    let Some(desired) =
        desired_citadel_turret_angles(pivot_world, target_world, mount_inverse, *current, limits)
    else {
        return false;
    };

    let Some(next_yaw) = move_citadel_turret_yaw_safely(
        current.yaw,
        desired.yaw,
        limits.yaw_speed * dt.max(0.0),
        limits,
    ) else {
        return false;
    };

    current.yaw = next_yaw;
    current.pitch = move_toward_citadel_turret(
        current.pitch,
        desired.pitch,
        limits.pitch_speed * dt.max(0.0),
    );
    true
}

/// Client-visible timeline for a harmless Cromatolis practice beam.
///
/// The leading edge travels out from the cannon muzzle. Once it has reached
/// the maximum range, the trailing edge follows it until the beam has fully
/// dissipated. This is visual-only; it carries no combat or collision state.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CitadelPracticeBeam {
    pub origin: Vec3<f32>,
    pub direction: Vec3<f32>,
    pub max_range: f32,
    pub speed: f32,
    /// Matches `LaserBeamLarge`: a half-metre-wide practice beam.
    pub diameter: f32,
}

impl CitadelPracticeBeam {
    pub fn travel_duration(&self) -> f32 { self.max_range / self.speed }

    pub fn total_duration(&self) -> f32 { self.travel_duration() * 2.0 }

    /// Returns the currently visible segment, or `None` once its tail has
    /// reached the leading edge at the maximum range.
    pub fn segment_at(&self, elapsed: f32) -> Option<(Vec3<f32>, Vec3<f32>)> {
        let travel_duration = self.travel_duration();
        let head_distance = (elapsed * self.speed).min(self.max_range);
        let tail_distance = ((elapsed - travel_duration) * self.speed).clamp(0.0, self.max_range);

        (tail_distance < self.max_range).then(|| {
            (
                self.origin + self.direction * tail_distance,
                self.origin + self.direction * head_distance,
            )
        })
    }
}

impl Component for CitadelPracticeBeam {
    type Storage = FlaggedStorage<Self, VecStorage<Self>>;
}

/// Client-visible timeline for the harmless Cromatolis practice sphere.
///
/// The server keeps this timeline entity at the cannon muzzle, allowing each
/// client to animate the sphere across its full range independently of entity
/// streaming or terrain chunk loading.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CitadelPracticeSphere {
    pub origin: Vec3<f32>,
    pub direction: Vec3<f32>,
    pub max_range: f32,
    pub speed: f32,
    pub diameter: f32,
}

impl CitadelPracticeSphere {
    pub fn travel_duration(&self) -> f32 { self.max_range / self.speed }

    pub fn position_at(&self, elapsed: f32) -> Option<Vec3<f32>> {
        (elapsed < self.travel_duration())
            .then(|| self.origin + self.direction * (elapsed * self.speed).min(self.max_range))
    }
}

impl Component for CitadelPracticeSphere {
    type Storage = FlaggedStorage<Self, VecStorage<Self>>;
}

#[cfg(test)]
mod tests {
    use super::{
        CitadelForceFieldShape, CitadelForceFieldVisual, CitadelPracticeBeam,
        CitadelPracticeSphere, CitadelTurretAngles, CitadelTurretLimits,
        desired_citadel_turret_angles, update_citadel_turret,
    };
    use std::f32::consts::PI;
    use vek::{Mat3, Vec3};

    #[test]
    fn pilot_tower_force_field_visual_has_the_authored_hollow_bubble_dimensions() {
        assert_eq!(
            CitadelForceFieldVisual::pilot_tower(),
            CitadelForceFieldVisual {
                shape: CitadelForceFieldShape::Dome,
                horizontal_radius: 14.0,
                height: 20.0,
            },
        );
    }

    #[test]
    fn lower_tower_safety_floor_is_collision_only_not_a_second_visible_field() {
        assert_eq!(
            CitadelForceFieldVisual::lower_platform().shape,
            CitadelForceFieldShape::SafetyFloor,
        );
    }

    #[test]
    fn practice_beam_extends_then_retracts_at_its_fixed_range() {
        let beam = CitadelPracticeBeam {
            origin: Vec3::zero(),
            direction: Vec3::unit_x(),
            max_range: 1_000.0,
            speed: 500.0,
            diameter: 0.5,
        };

        assert_eq!(
            beam.diameter, 0.5,
            "the citadel practice beam must use the LaserBeamLarge diameter",
        );

        assert_eq!(beam.segment_at(0.0), Some((Vec3::zero(), Vec3::zero())));
        assert_eq!(
            beam.segment_at(1.0),
            Some((Vec3::zero(), Vec3::new(500.0, 0.0, 0.0)))
        );
        assert_eq!(
            beam.segment_at(2.0),
            Some((Vec3::zero(), Vec3::new(1_000.0, 0.0, 0.0)))
        );
        assert_eq!(
            beam.segment_at(3.0),
            Some((Vec3::new(500.0, 0.0, 0.0), Vec3::new(1_000.0, 0.0, 0.0))),
        );
        assert_eq!(beam.segment_at(4.0), None);
    }

    #[test]
    fn practice_sphere_travels_the_full_fixed_range_from_the_muzzle() {
        let sphere = CitadelPracticeSphere {
            origin: Vec3::zero(),
            direction: Vec3::unit_x(),
            max_range: 1_000.0,
            speed: 500.0,
            diameter: 0.5,
        };

        assert_eq!(sphere.position_at(0.0), Some(Vec3::zero()));
        assert_eq!(sphere.position_at(1.0), Some(Vec3::new(500.0, 0.0, 0.0)));
        assert_eq!(sphere.position_at(2.0), None);
    }

    fn turret_limits(city_start: Option<f32>, city_end: Option<f32>) -> CitadelTurretLimits {
        CitadelTurretLimits {
            pitch_min: (-7.0_f32).to_radians(),
            pitch_max: 90.0_f32.to_radians(),
            city_sector_start: city_start,
            city_sector_end: city_end,
            yaw_speed: 10.0_f32.to_radians(),
            pitch_speed: 10.0_f32.to_radians(),
        }
    }

    #[test]
    fn turret_rejects_a_target_in_a_city_sector_that_wraps_at_pi() {
        let limits = turret_limits(
            Some(150.0_f32.to_radians()),
            Some((-150.0_f32).to_radians()),
        );

        assert!(
            desired_citadel_turret_angles(
                Vec3::zero(),
                Vec3::new(0.0, 0.0, -10.0),
                Mat3::identity(),
                CitadelTurretAngles::default(),
                &limits,
            )
            .is_none()
        );
    }

    #[test]
    fn turret_uses_the_long_yaw_path_when_the_short_path_crosses_the_city() {
        let limits = turret_limits(
            Some(150.0_f32.to_radians()),
            Some((-150.0_f32).to_radians()),
        );
        let mut angles = CitadelTurretAngles {
            yaw: 120.0_f32.to_radians(),
            pitch: 0.0,
        };
        let target_yaw = (-120.0_f32).to_radians();
        let target = Vec3::new(target_yaw.sin() * 10.0, 0.0, target_yaw.cos() * 10.0);

        assert!(update_citadel_turret(
            &mut angles,
            Vec3::zero(),
            target,
            Mat3::identity(),
            &limits,
            1.0,
        ));
        assert!(
            angles.yaw < 120.0_f32.to_radians(),
            "the safe long path begins clockwise, away from the city sector",
        );
        assert!(angles.yaw > -PI);
    }

    #[test]
    fn turret_accepts_the_practical_upward_range_below_its_90_degree_pitch_limit() {
        let limits = turret_limits(None, None);
        // Exactly vertical has no horizontal bearing and is rejected before
        // limit selection; 89° proves the new 90° upper limit admits the
        // complete practical upward range.
        let elevation = 89.0_f32.to_radians();
        let target = Vec3::new(0.0, elevation.sin() * 10.0, elevation.cos() * 10.0);

        let angles = desired_citadel_turret_angles(
            Vec3::zero(),
            target,
            Mat3::identity(),
            CitadelTurretAngles {
                yaw: PI,
                pitch: 89.0_f32.to_radians(),
            },
            &limits,
        )
        .expect("the practical upward representation below 90 degrees must be valid");

        assert!((limits.pitch_max - 90.0_f32.to_radians()).abs() < 0.001);
        assert!((angles.pitch - 89.0_f32.to_radians()).abs() < 0.001);
    }

    #[test]
    fn turret_accepts_the_minus_seven_degree_limit_but_rejects_targets_below_it() {
        let limits = turret_limits(None, None);
        let target_at_limit = (-7.0_f32).to_radians();
        let target_below_limit = (-8.0_f32).to_radians();
        let point_at =
            |elevation: f32| Vec3::new(0.0, elevation.sin() * 10.0, elevation.cos() * 10.0);

        let at_limit = desired_citadel_turret_angles(
            Vec3::zero(),
            point_at(target_at_limit),
            Mat3::identity(),
            CitadelTurretAngles::default(),
            &limits,
        )
        .expect("the cannon must be able to aim at the -7 degree limit");
        assert!((at_limit.pitch - target_at_limit).abs() < 0.001);

        assert!(
            desired_citadel_turret_angles(
                Vec3::zero(),
                point_at(target_below_limit),
                Mat3::identity(),
                CitadelTurretAngles::default(),
                &limits,
            )
            .is_none()
        );
    }
}
