use super::{
    super::{Animation, vek::*},
    ObjectSkeleton, SkeletonAttr,
};
use common::comp::{CitadelTurretAngles, object::Body};

/// Presents the server-synchronised articulated pose of a Cromatolis Aerial
/// Citadel defence cannon.
///
/// `bone0` is the yaw carriage: it turns visibly around the model's local
/// vertical Z axis. `bone1` repeats that yaw and adds its local X-axis pitch,
/// keeping the tube hinged at the authored trunnion. The entity itself remains
/// unrotated; this avoids losing the yaw to generic object interpolation.
///
/// `angles` arrives here already in **model space** (local Y = forward at
/// yaw zero, local X = pitch axis after yaw) -- see
/// `common::comp::citadel`'s pose-space doc comments and
/// `server::citadel::upper_turret_rest_pose`/`pilot_pose_from_operator_degrees`
/// for where author-space and operator-space angles get converted into this
/// space before reaching the component this animation reads.
pub struct TurretAnimation;

impl Animation for TurretAnimation {
    type Dependency<'a> = (CitadelTurretAngles, Body);
    type Skeleton = ObjectSkeleton;

    #[cfg(feature = "use-dyn-lib")]
    const UPDATE_FN: &'static [u8] = b"object_turret\0";

    #[cfg_attr(feature = "be-dyn-lib", unsafe(export_name = "object_turret"))]
    fn update_skeleton_inner(
        skeleton: &Self::Skeleton,
        (angles, body): Self::Dependency<'_>,
        _anim_time: f32,
        _rate: &mut f32,
        s_a: &SkeletonAttr,
    ) -> Self::Skeleton {
        let mut next = (*skeleton).clone();
        next.bone0.position = Vec3::new(s_a.bone0.0, s_a.bone0.1, s_a.bone0.2);
        next.bone1.position = Vec3::new(s_a.bone1.0, s_a.bone1.1, s_a.bone1.2);

        if matches!(
            body,
            Body::CitadelArcaneCannon | Body::CitadelArcaneSphereCannon
        ) {
            // Model-space rotation: in the authored model +Y is forward and
            // +Z is up. Positive model yaw turns the tube toward +X (see
            // `server::citadel::upper_turret_rest_pose`'s
            // `atan2(outward.x, outward.y)` derivation), so the carriage
            // orientation here negates the stored angle to match the
            // renderer's left-handed rotation convention around +Z.
            let yaw = Quaternion::rotation_z(-angles.yaw);
            next.bone0.orientation = yaw;
            // +pitch lifts the +Y-forward tube toward +Z after its yaw.
            next.bone1.orientation = yaw * Quaternion::rotation_x(angles.pitch);
        }

        next
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn positive_citadel_pitch_lifts_the_barrel_above_its_forward_axis() {
        let mut rate = 0.0;
        let skeleton = TurretAnimation::update_skeleton(
            &ObjectSkeleton::default(),
            (
                CitadelTurretAngles {
                    yaw: 0.0,
                    pitch: std::f32::consts::FRAC_PI_4,
                },
                Body::CitadelArcaneCannon,
            ),
            0.0,
            &mut rate,
            &SkeletonAttr::default(),
        );

        let forward = skeleton.bone1.orientation * Vec3::unit_y();
        assert!(forward.z > 0.7, "positive pitch must lift the barrel");
    }

    #[test]
    fn negative_citadel_pitch_drops_the_lower_station_barrel_into_the_precipice() {
        let mut rate = 0.0;
        let skeleton = TurretAnimation::update_skeleton(
            &ObjectSkeleton::default(),
            (
                CitadelTurretAngles {
                    yaw: 0.0,
                    pitch: -std::f32::consts::FRAC_PI_2,
                },
                Body::CitadelArcaneSphereCannon,
            ),
            0.0,
            &mut rate,
            &SkeletonAttr::default(),
        );

        let forward = skeleton.bone1.orientation * Vec3::unit_y();
        assert!(
            forward.z < -0.999,
            "the lower cannon barrel must point vertically down",
        );
    }

    #[test]
    fn citadel_yaw_turns_both_the_carriage_and_barrel_horizontally() {
        let mut rate = 0.0;
        let skeleton = TurretAnimation::update_skeleton(
            &ObjectSkeleton::default(),
            (
                CitadelTurretAngles {
                    yaw: std::f32::consts::FRAC_PI_2,
                    pitch: 0.0,
                },
                Body::CitadelArcaneCannon,
            ),
            0.0,
            &mut rate,
            &SkeletonAttr::default(),
        );

        let expected = Vec3::unit_x();
        let carriage_forward = skeleton.bone0.orientation * Vec3::unit_y();
        let barrel_forward = skeleton.bone1.orientation * Vec3::unit_y();
        assert!(carriage_forward.dot(expected) > 0.999);
        assert!(barrel_forward.dot(expected) > 0.999);
    }

    #[test]
    fn citadel_barrel_pivot_matches_the_base_trunnion_height() {
        let attr = SkeletonAttr::from(&Body::CitadelArcaneCannon);
        assert_eq!(attr.bone1, (0.0, 0.0, 17.0));
    }

    #[test]
    fn non_citadel_object_bodies_keep_the_legacy_decoupled_bone1_orientation() {
        // A conventional object (e.g. a Crossbow turret) must not pick up
        // the citadel-only `bone0`-composed pitch behavior.
        let mut rate = 0.0;
        let skeleton = TurretAnimation::update_skeleton(
            &ObjectSkeleton::default(),
            (
                CitadelTurretAngles {
                    yaw: std::f32::consts::FRAC_PI_2,
                    pitch: std::f32::consts::FRAC_PI_4,
                },
                Body::Crossbow,
            ),
            0.0,
            &mut rate,
            &SkeletonAttr::default(),
        );

        // Non-citadel bodies never touch `bone0`/`bone1` orientation in this
        // animation, so both stay at the skeleton's default (identity).
        assert_eq!(skeleton.bone0.orientation, Quaternion::identity());
        assert_eq!(skeleton.bone1.orientation, Quaternion::identity());
    }
}
