//! Server-owned placement of Cromatolis' authored upper defensive stations.
//!
//! The terrain layer owns tower geometry; this module creates the physical
//! voxel cannon and transparent force-field entities at that geometry's
//! canonical centres.

use crate::state_ext::StateExt;
use common::comp::{
    self, CitadelForceFieldShape, CitadelForceFieldVisual, CitadelTurretAngles,
    citadel::AuthoredCitadelAerialFeatures,
};
use common_state::State;
use specs::{Builder, Entity as EcsEntity, Join, WorldExt};
use vek::{Vec2, Vec3};

// Older pilot commands placed stationary cannons slightly outward from their
// tower centres. Capture only those local legacy entities during recovery;
// neighbouring perimeter towers are much farther apart.
//
// This radius assumes no two authored tower anchors sit within
// `2 * UPPER_TURRET_RECOVERY_RADIUS` of each other -- otherwise a legacy
// cannon near the midpoint between two towers could be claimed as a
// recovery candidate for the wrong one. The 24 authored towers are spaced
// 78-116 m apart (see `cromatolis_v0_aerial_features.ron`'s notes), well
// clear of this 64 m floor.
const UPPER_TURRET_RECOVERY_RADIUS: f32 = 32.0;
const UPPER_TURRET_RECOVERY_VERTICAL_TOLERANCE: f32 = 16.0;
// A legacy lower cannon was created at the platform level with its complete
// entity upside down. Its corrected physical mount is one cannon height below
// that level, so recovery must span exactly that short migration distance.
const LOWER_TURRET_RECOVERY_VERTICAL_TOLERANCE: f32 = 8.0;

fn component_needs_restore<T: PartialEq>(current: Option<&T>, desired: &T) -> bool {
    current != Some(desired)
}

/// Loads the authored force-field dimensions backing every preset below.
/// Fails hard on a missing/invalid asset, matching
/// `AuthoredCitadelAerialFeatures::validate`'s existing "fail hard rather
/// than skip-and-warn" policy -- silently wrong field dimensions on a live
/// citadel are worse than a loud panic during placement/reconciliation.
/// `assets_manager` caches the parsed RON internally, so calling this
/// repeatedly (once per tower, at 1Hz) is cheap.
fn aerial_features() -> AuthoredCitadelAerialFeatures {
    let features = AuthoredCitadelAerialFeatures::load_owned()
        .expect("Cromatolis aerial features asset must be present and parse as valid RON");
    features
        .validate()
        .expect("Cromatolis aerial features asset must match the expected schema");
    features
}

pub(crate) fn upper_turret_body(tower_index: usize) -> comp::object::Body {
    if tower_index.is_multiple_of(2) {
        comp::object::Body::CitadelArcaneCannon
    } else {
        comp::object::Body::CitadelArcaneSphereCannon
    }
}

pub(crate) fn upper_turret_center(tower_index: usize) -> Vec2<f32> {
    let center =
        world::layer::cromatolis_aerial_citadel::cromatolis_aerial_citadel_wall_tower_world_center(
            tower_index,
        )
        .expect("citadel upper cannon must reference an authored tower");
    Vec2::new(center.x as f32, center.y as f32)
}

pub(crate) fn upper_turret_pivot(tower_index: usize) -> Vec3<f32> {
    let deck_z =
        world::layer::cromatolis_aerial_citadel::cromatolis_aerial_citadel_wall_tower_watch_deck_z(
            tower_index,
        )
        .expect("citadel upper cannon must reference an authored watch deck") as f32;
    let features = aerial_features();
    let clearance = match upper_turret_body(tower_index) {
        comp::object::Body::CitadelArcaneCannon => features.pilot_beam_pivot_clearance_m,
        comp::object::Body::CitadelArcaneSphereCannon => features.pilot_sphere_pivot_clearance_m,
        _ => unreachable!("upper citadel stations have one of the two cannon bodies"),
    };
    upper_turret_center(tower_index).with_z(deck_z + clearance)
}

pub(crate) fn upper_turret_rest_pose(tower_index: usize) -> CitadelTurretAngles {
    let outward =
        world::layer::cromatolis_aerial_citadel::cromatolis_aerial_citadel_wall_tower_outward_axis(
            tower_index,
        )
        .expect("citadel upper cannon must reference an authored outward axis");
    CitadelTurretAngles {
        // The authored barrel points +Y at yaw zero. `atan2(x, y)` therefore
        // maps each tower's cardinal outward vector into the model convention.
        yaw: (outward.x as f32).atan2(outward.y as f32),
        pitch: 0.0,
    }
}

/// `pivot`/`expected_body` are the caller's already-resolved values for this
/// tower (each `upper_turret_pivot` call re-loads and clones the authored
/// tower table, so this predicate must never resolve them itself: it runs
/// once per `Immovable`+`Body`+`Pos` entity in the *entire world* -- every
/// campfire, portal, and totem, not just the 24 towers -- inside the caller's
/// `.join().filter_map(...)`).
fn upper_turret_is_recovery_candidate(
    pivot: Vec3<f32>,
    expected_body: comp::Body,
    body: comp::Body,
    position: Vec3<f32>,
) -> bool {
    body == expected_body
        && (position.xy() - pivot.xy()).magnitude_squared() <= UPPER_TURRET_RECOVERY_RADIUS.powi(2)
        && (position.z - pivot.z).abs() <= UPPER_TURRET_RECOVERY_VERTICAL_TOLERANCE
}

fn terrain_home_chunk(state: &State, pos: Vec3<f32>) -> Option<Vec2<i32>> {
    let terrain = state.terrain();
    let chunk = terrain.pos_key(pos.map(|axis| axis.floor() as i32));
    terrain.get_key_real(chunk).is_some().then_some(chunk)
}

fn spawn_upper_defence(state: &mut State, tower_index: usize, home_chunk: Vec2<i32>) {
    state
        .create_object(
            comp::Pos(upper_turret_pivot(tower_index)),
            upper_turret_body(tower_index),
        )
        .with(comp::Ori::default())
        .with(comp::Immovable)
        // Static authored stations must be anchored to a loaded terrain
        // chunk. Without this, entities created during server construction
        // are removed by the first unloaded-chunk cleanup before a client can
        // ever receive them.
        .with(comp::Anchor::Chunk(home_chunk))
        .with(aerial_features().upper_tower())
        .with(upper_turret_rest_pose(tower_index))
        .build();
}

/// Ensures the complete upper ring is live without duplicating a station
/// that was already synchronized to connected players. It runs after terrain
/// streaming and is also safe to call from a pilot control command: that
/// gives a live world a deterministic recovery path after a hot-reload.
///
/// Existing station poses are intentionally left untouched. A pilot can keep
/// aiming while an absent station or its force-field visual is restored.
pub(crate) fn ensure_upper_defences(state: &mut State) -> usize {
    let home_chunks: Vec<Option<Vec2<i32>>> = (0
        ..world::layer::cromatolis_aerial_citadel::cromatolis_aerial_citadel_wall_tower_count())
        .map(|tower_index| terrain_home_chunk(state, upper_turret_pivot(tower_index)))
        .collect();

    let (existing, duplicate_entities): (Vec<Option<EcsEntity>>, Vec<EcsEntity>) = {
        let ecs = state.ecs();
        let entities = ecs.entities();
        let bodies = ecs.read_storage::<comp::Body>();
        let positions = ecs.read_storage::<comp::Pos>();
        let immovables = ecs.read_storage::<comp::Immovable>();

        let mut duplicates = Vec::new();
        let stations = (0
            ..world::layer::cromatolis_aerial_citadel::cromatolis_aerial_citadel_wall_tower_count(
            ))
            .map(|tower_index| {
                let pivot = upper_turret_pivot(tower_index);
                let expected_body = comp::Body::Object(upper_turret_body(tower_index));
                let mut candidates = (&entities, &bodies, &positions, &immovables)
                    .join()
                    .filter_map(|(entity, body, pos, _)| {
                        upper_turret_is_recovery_candidate(pivot, expected_body, *body, pos.0)
                            .then_some((entity, pos.0))
                    })
                    .collect::<Vec<_>>();
                candidates.sort_by(|(_, left), (_, right)| {
                    left.distance_squared(pivot)
                        .total_cmp(&right.distance_squared(pivot))
                });
                if let Some((entity, _)) = candidates.first() {
                    duplicates.extend(candidates.iter().skip(1).map(|(entity, _)| *entity));
                    Some(*entity)
                } else {
                    None
                }
            })
            .collect();
        (stations, duplicates)
    };

    // A previous implementation could leave an outward-offset cannon behind
    // when it introduced the centered station. Retain the closest one and
    // remove only extra immovable citadel cannons captured by the narrow
    // recovery radius, preventing superimposed models and force fields.
    for entity in duplicate_entities {
        state
            .delete_entity_recorded(entity)
            .expect("duplicate static citadel cannon must remain a valid entity during recovery");
    }

    let positions_to_restore: Vec<(EcsEntity, Vec3<f32>)> = {
        let ecs = state.ecs();
        let positions = ecs.read_storage::<comp::Pos>();
        existing
            .iter()
            .enumerate()
            .filter_map(|(tower_index, entity)| {
                let entity = (*entity)?;
                let pivot = upper_turret_pivot(tower_index);
                (positions.get(entity).copied() != Some(comp::Pos(pivot)))
                    .then_some((entity, pivot))
            })
            .collect()
    };
    if !positions_to_restore.is_empty() {
        let mut positions = state.ecs_mut().write_storage::<comp::Pos>();
        for (entity, pivot) in positions_to_restore {
            positions
                .insert(entity, comp::Pos(pivot))
                .expect("existing citadel cannon must accept its centered position");
        }
    }

    // A previously-created physical cannon may have survived a hot reload
    // without its new field component. Restore that visual in place instead
    // of replacing the entity and discarding its current yaw/pitch.
    let desired_field = aerial_features().upper_tower();
    let fields_to_restore: Vec<EcsEntity> = {
        let ecs = state.ecs();
        let fields = ecs.read_storage::<CitadelForceFieldVisual>();
        existing
            .iter()
            .flatten()
            .filter(|entity| component_needs_restore(fields.get(**entity), &desired_field))
            .copied()
            .collect()
    };
    if !fields_to_restore.is_empty() {
        let mut fields = state.ecs_mut().write_storage::<CitadelForceFieldVisual>();
        for entity in fields_to_restore {
            fields
                .insert(entity, desired_field)
                .expect("existing citadel cannon entity must accept its force field");
        }
    }

    let mut spawned = 0;
    for (tower_index, (entity, home_chunk)) in existing.into_iter().zip(home_chunks).enumerate() {
        if let (None, Some(home_chunk)) = (entity, home_chunk) {
            spawn_upper_defence(state, tower_index, home_chunk);
            spawned += 1;
        }
    }
    spawned
}

fn lower_tower_force_field_pivot(lower_tower_index: usize) -> Vec3<f32> {
    let platform = world::layer::cromatolis_aerial_citadel::cromatolis_aerial_citadel_lower_tower_platform_center(
        lower_tower_index,
    )
    .expect("each authored lower citadel tower must have a platform centre");
    Vec3::new(platform.x as f32, platform.y as f32, platform.z as f32)
}

/// The structural cannon platform occupies the level immediately above the
/// energy floor. Keeping the collider one block lower lets the final stair
/// enter the lookout normally, while still catching a player over the open
/// centre of the lower tower.
fn lower_tower_safety_floor_pivot(lower_tower_index: usize) -> Vec3<f32> {
    lower_tower_force_field_pivot(lower_tower_index) - Vec3::unit_z()
}

/// The lower ring always reverses its parent upper tower's weapon type.
/// Thus neighbouring upper/lower stations cannot carry the same cannon.
pub(crate) fn lower_tower_turret_body(lower_tower_index: usize) -> comp::object::Body {
    let parent_tower_index =
        world::layer::cromatolis_aerial_citadel::cromatolis_aerial_citadel_lower_tower_upper_index(
            lower_tower_index,
        )
        .expect("each lower station must have an authored parent tower");
    match upper_turret_body(parent_tower_index) {
        comp::object::Body::CitadelArcaneCannon => comp::object::Body::CitadelArcaneSphereCannon,
        comp::object::Body::CitadelArcaneSphereCannon => comp::object::Body::CitadelArcaneCannon,
        _ => unreachable!("upper citadel stations have one of the two cannon bodies"),
    }
}

/// Object-body collision begins at `Pos` and ends at `Pos + height`. Mounting
/// the lower cannon one body-height below its platform keeps the physical base
/// bolted to the intended underside without intersecting the platform.
pub(crate) fn lower_tower_turret_pivot(lower_tower_index: usize) -> Vec3<f32> {
    let cannon = comp::Body::Object(lower_tower_turret_body(lower_tower_index));
    lower_tower_force_field_pivot(lower_tower_index) - Vec3::unit_z() * cannon.height()
}

/// `pivot`/`expected_body` are the caller's already-resolved values for this
/// lower tower -- see `upper_turret_is_recovery_candidate`'s doc comment for
/// why this predicate must never resolve them itself.
fn lower_tower_turret_is_recovery_candidate(
    pivot: Vec3<f32>,
    expected_body: comp::Body,
    body: comp::Body,
    position: Vec3<f32>,
) -> bool {
    body == expected_body
        && (position.xy() - pivot.xy()).magnitude_squared() < 0.01
        && (position.z - pivot.z).abs() <= LOWER_TURRET_RECOVERY_VERTICAL_TOLERANCE
}

pub(crate) fn lower_tower_turret_rest_pose(_lower_tower_index: usize) -> CitadelTurretAngles {
    CitadelTurretAngles {
        yaw: 0.0,
        // Keep the physical base upright. The animated barrel uses the
        // regular local pitch joint, where negative pitch points +Y forward
        // toward world-space down.
        pitch: -std::f32::consts::FRAC_PI_2,
    }
}

/// An invisible physical safety plate beneath the lower platform. A capsule
/// prism expresses the intended flat collision directly, without allocating
/// mutable voxel geometry. It also has no model of its own, keeping this
/// collision-only entity separate from the dome and cannon visuals.
///
/// `radius`/`thickness` must come from the same authored source the visible
/// `CitadelForceFieldVisual::SafetyFloor` uses (`aerial_features()`'s
/// `lower_platform().horizontal_radius`/`lower_safety_floor_thickness_m`) --
/// never a separate Rust constant, or the visual and physical footprints can
/// silently drift apart the next time the RON asset is retuned.
fn lower_tower_force_field_collider(radius: f32, thickness: f32) -> comp::Collider {
    comp::Collider::CapsulePrism(comp::CapsulePrism {
        // A stadium shape wide enough to cover the centre opening and
        // deliberately lower than the authored stone platform.
        p0: Vec2::new(-0.5, 0.0),
        p1: Vec2::new(0.5, 0.0),
        radius,
        z_min: 0.0,
        z_max: thickness,
    })
}

fn lower_tower_force_field_has_correct_collider(
    collider: Option<&comp::Collider>,
    radius: f32,
    thickness: f32,
) -> bool {
    matches!(
        collider,
        Some(comp::Collider::CapsulePrism(comp::CapsulePrism {
            p0,
            p1,
            radius: actual_radius,
            z_min,
            z_max,
        })) if *p0 == Vec2::new(-0.5, 0.0)
            && *p1 == Vec2::new(0.5, 0.0)
            && *actual_radius == radius
            && *z_min == 0.0
            && *z_max == thickness
    )
}

/// Ensures the collision-only safety floor for one lower tower exists once.
/// The authored stone platform remains visible above this safety net.
///
/// Unlike the cannon-placement paths above, this does not hunt for and
/// delete duplicate entities: the legacy off-center placement bug that
/// motivated that dedup logic only ever affected the *cannons* (an old pilot
/// command placed them slightly outward from their tower centres). No
/// equivalent legacy-duplicate scenario exists for the safety floor or dome
/// below, since neither was ever separately, manually placeable.
fn ensure_lower_tower_force_field(state: &mut State, lower_tower_index: usize) -> bool {
    let pivot = lower_tower_safety_floor_pivot(lower_tower_index);
    let Some(home_chunk) = terrain_home_chunk(state, pivot) else {
        return false;
    };
    let existing = {
        let ecs = state.ecs();
        let entities = ecs.entities();
        let fields = ecs.read_storage::<CitadelForceFieldVisual>();
        let positions = ecs.read_storage::<comp::Pos>();
        (&entities, &fields, &positions)
            .join()
            .find_map(|(entity, field, pos)| {
                (field.shape == CitadelForceFieldShape::SafetyFloor
                    && (pos.0.xy() - pivot.xy()).magnitude_squared() < 0.01
                    // Accept the pre-fix floor one block above the intended
                    // level, then migrate it below the stairs instead of
                    // leaving a second overlapping physical plate behind.
                    && (pos.0.z - pivot.z).abs() <= 1.1)
                    .then_some(entity)
            })
    };

    let features = aerial_features();
    let desired_field = features.lower_platform();
    let collider_radius = desired_field.horizontal_radius;
    let collider_thickness = features.lower_safety_floor_thickness_m;

    if let Some(entity) = existing {
        let (needs_position, needs_field, needs_collider) = {
            let ecs = state.ecs();
            (
                ecs.read_storage::<comp::Pos>().get(entity).copied() != Some(comp::Pos(pivot)),
                component_needs_restore(
                    ecs.read_storage::<CitadelForceFieldVisual>().get(entity),
                    &desired_field,
                ),
                !lower_tower_force_field_has_correct_collider(
                    ecs.read_storage::<comp::Collider>().get(entity),
                    collider_radius,
                    collider_thickness,
                ),
            )
        };
        if needs_position {
            state
                .ecs_mut()
                .write_storage::<comp::Pos>()
                .insert(entity, comp::Pos(pivot))
                .expect("the existing lower safety-floor entity must accept its corrected height");
        }
        if needs_field {
            state
                .ecs_mut()
                .write_storage::<CitadelForceFieldVisual>()
                .insert(entity, desired_field)
                .expect("the existing lower safety-floor entity must accept its visual marker");
        }
        if needs_collider {
            state
                .ecs_mut()
                .write_storage::<comp::Collider>()
                .insert(
                    entity,
                    lower_tower_force_field_collider(collider_radius, collider_thickness),
                )
                .expect("the existing lower safety-floor entity must accept its collider");
        }
        return false;
    }

    state
        .create_empty(comp::Pos(pivot))
        .with(comp::Immovable)
        .with(comp::Anchor::Chunk(home_chunk))
        .with(lower_tower_force_field_collider(
            collider_radius,
            collider_thickness,
        ))
        .with(desired_field)
        .build();
    true
}

fn spawn_lower_tower_defence(state: &mut State, lower_tower_index: usize, home_chunk: Vec2<i32>) {
    state
        .create_object(
            comp::Pos(lower_tower_turret_pivot(lower_tower_index)),
            lower_tower_turret_body(lower_tower_index),
        )
        .with(comp::Ori::default())
        .with(comp::Immovable)
        .with(comp::Anchor::Chunk(home_chunk))
        .with(lower_tower_turret_rest_pose(lower_tower_index))
        .build();
}

/// The dome is visual-only and remains at the structural platform centre.
/// Decoupling it from the cannon means aiming cannot translate or rotate the
/// protection shell, and it avoids mixing a mesh-only effect with physics.
///
/// Like `ensure_lower_tower_force_field` above, this does not dedupe: no
/// legacy manually-placed dome ever existed to leave a duplicate behind.
fn ensure_lower_tower_dome(state: &mut State, lower_tower_index: usize) -> bool {
    let pivot = lower_tower_force_field_pivot(lower_tower_index);
    let Some(home_chunk) = terrain_home_chunk(state, pivot) else {
        return false;
    };
    let desired_field = aerial_features().lower_tower_dome();
    let exists = {
        let ecs = state.ecs();
        let fields = ecs.read_storage::<CitadelForceFieldVisual>();
        let positions = ecs.read_storage::<comp::Pos>();
        (&fields, &positions).join().any(|(field, pos)| {
            *field == desired_field && (pos.0 - pivot).magnitude_squared() < 0.01
        })
    };
    if exists {
        return false;
    }

    state
        .create_empty(comp::Pos(pivot))
        .with(comp::Immovable)
        .with(comp::Anchor::Chunk(home_chunk))
        .with(desired_field)
        .build();
    true
}

/// Ensures an under-island cannon fires below its upper partner. The
/// safety floor, visual dome, and physical cannon are independent entities:
/// this prevents render-only field geometry or a rotating barrel from
/// disturbing player collision.
fn ensure_lower_tower_defence(state: &mut State, lower_tower_index: usize) -> bool {
    let pivot = lower_tower_turret_pivot(lower_tower_index);
    let Some(home_chunk) = terrain_home_chunk(state, pivot) else {
        return false;
    };
    let desired_pose = lower_tower_turret_rest_pose(lower_tower_index);
    let (existing, duplicates) = {
        let ecs = state.ecs();
        let entities = ecs.entities();
        let bodies = ecs.read_storage::<comp::Body>();
        let positions = ecs.read_storage::<comp::Pos>();
        let immovables = ecs.read_storage::<comp::Immovable>();
        let expected_body = comp::Body::Object(lower_tower_turret_body(lower_tower_index));
        let mut candidates = (&entities, &bodies, &positions, &immovables)
            .join()
            .filter_map(|(entity, body, pos, _)| {
                lower_tower_turret_is_recovery_candidate(pivot, expected_body, *body, pos.0)
                    .then_some((entity, pos.0))
            })
            .collect::<Vec<_>>();
        candidates.sort_by(|(_, left), (_, right)| {
            left.distance_squared(pivot)
                .total_cmp(&right.distance_squared(pivot))
        });
        let existing = candidates.first().map(|(entity, _)| *entity);
        let duplicates = candidates
            .iter()
            .skip(1)
            .map(|(entity, _)| *entity)
            .collect::<Vec<_>>();
        (existing, duplicates)
    };

    for entity in duplicates {
        state
            .delete_entity_recorded(entity)
            .expect("duplicate lower citadel cannon must remain valid during recovery");
    }

    let cannon_changed = if let Some(entity) = existing {
        let (needs_pos, needs_pose, needs_ori, has_legacy_field) = {
            let ecs = state.ecs();
            (
                ecs.read_storage::<comp::Pos>().get(entity).copied() != Some(comp::Pos(pivot)),
                component_needs_restore(
                    ecs.read_storage::<CitadelTurretAngles>().get(entity),
                    &desired_pose,
                ),
                ecs.read_storage::<comp::Ori>().get(entity).copied() != Some(comp::Ori::default()),
                ecs.read_storage::<CitadelForceFieldVisual>()
                    .get(entity)
                    .is_some(),
            )
        };
        if needs_pos {
            state
                .ecs_mut()
                .write_storage::<comp::Pos>()
                .insert(entity, comp::Pos(pivot))
                .expect("the existing lower cannon must accept its corrected mount position");
        }
        if needs_pose {
            state
                .ecs_mut()
                .write_storage::<CitadelTurretAngles>()
                .insert(entity, desired_pose)
                .expect("the existing lower cannon must accept its rest pose");
        }
        if needs_ori {
            state
                .ecs_mut()
                .write_storage::<comp::Ori>()
                .insert(entity, comp::Ori::default())
                .expect("the existing lower cannon must accept its upright physical orientation");
        }
        if has_legacy_field {
            state
                .ecs_mut()
                .write_storage::<CitadelForceFieldVisual>()
                .remove(entity);
        }
        needs_pos || needs_pose || needs_ori || has_legacy_field
    } else {
        spawn_lower_tower_defence(state, lower_tower_index, home_chunk);
        true
    };

    // Do not defer the shell to a later refresh when the cannon was just
    // spawned: a newly streamed lower tower must arrive as one coherent
    // station (platform, cannon, and dome) in the same server tick.
    let dome_changed = ensure_lower_tower_dome(state, lower_tower_index);
    cannon_changed || dome_changed
}

/// Materializes every approved lower station only while its own terrain chunk
/// is live. Each station is independently recovered by its exact platform
/// pivot, so streaming tower one can never replace or move tower zero.
pub(crate) fn ensure_lower_tower_defences(state: &mut State) -> usize {
    let mut changes = 0;
    for lower_tower_index in
        0..world::layer::cromatolis_aerial_citadel::cromatolis_aerial_citadel_lower_tower_count()
    {
        changes += usize::from(ensure_lower_tower_force_field(state, lower_tower_index));
        changes += usize::from(ensure_lower_tower_defence(state, lower_tower_index));
    }
    changes
}

#[cfg(test)]
mod tests {
    use super::{
        component_needs_restore, lower_tower_force_field_collider, lower_tower_safety_floor_pivot,
        lower_tower_turret_body, lower_tower_turret_is_recovery_candidate,
        lower_tower_turret_pivot, lower_tower_turret_rest_pose, upper_turret_body,
        upper_turret_center, upper_turret_is_recovery_candidate, upper_turret_pivot,
        upper_turret_rest_pose,
    };
    use common::{
        comp,
        comp::{CitadelForceFieldShape, CitadelForceFieldVisual, object::Body},
    };
    use vek::{Vec2, Vec3};

    #[test]
    fn every_upper_station_uses_its_tower_centre_and_outward_rest_pose() {
        let count =
            world::layer::cromatolis_aerial_citadel::cromatolis_aerial_citadel_wall_tower_count();
        assert_eq!(count, 24);

        for tower_index in 0..count {
            let world_center = world::layer::cromatolis_aerial_citadel::cromatolis_aerial_citadel_wall_tower_world_center(tower_index)
                    .expect("the index is inside the authored tower table");
            assert_eq!(
                upper_turret_center(tower_index),
                Vec2::new(world_center.x as f32, world_center.y as f32),
            );
            assert_eq!(
                upper_turret_pivot(tower_index).xy(),
                upper_turret_center(tower_index)
            );
            assert_eq!(upper_turret_rest_pose(tower_index).pitch, 0.0);
            assert_eq!(
                upper_turret_body(tower_index),
                if tower_index.is_multiple_of(2) {
                    Body::CitadelArcaneCannon
                } else {
                    Body::CitadelArcaneSphereCannon
                },
            );
        }
    }

    /// The documented contract states "odd tower number -> beam,
    /// even -> fireball/sphere" using ONE-based tower numbers (tower 1,
    /// tower 2, ...). The implementation instead keys off
    /// `tower_index.is_multiple_of(2)` on the ZERO-based `tower_index`
    /// parameter. Because consecutive integers alternate parity,
    /// zero-based-even (0, 2, 4, ...) is exactly one-based-odd (1, 3, 5,
    /// ...), so this *is* a correct porting of the documented contract --
    /// but a careless re-implementation that keyed off `tower_index` odd/even
    /// as though `tower_index` were already the one-based tower number would
    /// silently invert every tower's weapon. This test locks in the actual,
    /// zero-based-index behavior against explicit one-based tower numbers.
    #[test]
    fn upper_turret_alternation_matches_the_documented_contract_via_zero_based_index_parity() {
        let one_based_towers_and_expected_body = [
            (1, Body::CitadelArcaneCannon),       // odd -> beam
            (2, Body::CitadelArcaneSphereCannon), // even -> sphere
            (3, Body::CitadelArcaneCannon),
            (4, Body::CitadelArcaneSphereCannon),
            (23, Body::CitadelArcaneCannon),
            (24, Body::CitadelArcaneSphereCannon),
        ];

        for (one_based_tower_number, expected_body) in one_based_towers_and_expected_body {
            let tower_index = one_based_tower_number - 1;
            assert_eq!(
                upper_turret_body(tower_index),
                expected_body,
                "documented (one-based) tower {one_based_tower_number} must resolve through \
                 zero-based index {tower_index}",
            );
            // The zero-based predicate the implementation actually uses --
            // NOT `tower_index` treated as if it were the one-based number.
            assert_eq!(
                tower_index.is_multiple_of(2),
                expected_body == Body::CitadelArcaneCannon,
                "beam selection must key off zero-based index parity, not a naive one-based \
                 reading of `tower_index`",
            );
        }
    }

    #[test]
    fn upper_station_recovery_migrates_only_nearby_legacy_cannons() {
        let tower_index = 0;
        let pivot = upper_turret_pivot(tower_index);
        let expected_body = comp::Body::Object(upper_turret_body(tower_index));

        assert!(upper_turret_is_recovery_candidate(
            pivot,
            expected_body,
            expected_body,
            pivot + Vec3::new(12.0, 0.0, 0.0),
        ));
        assert!(!upper_turret_is_recovery_candidate(
            pivot,
            expected_body,
            expected_body,
            pivot + Vec3::new(33.0, 0.0, 0.0),
        ));
        assert!(!upper_turret_is_recovery_candidate(
            pivot,
            expected_body,
            expected_body,
            pivot + Vec3::new(0.0, 0.0, 17.0),
        ));
    }

    #[test]
    fn every_lower_force_field_uses_its_authored_tower_platform() {
        for lower_tower_index in 0
            ..world::layer::cromatolis_aerial_citadel::cromatolis_aerial_citadel_lower_tower_count()
        {
            let parent_tower_index = world::layer::cromatolis_aerial_citadel::cromatolis_aerial_citadel_lower_tower_upper_index(
                    lower_tower_index,
                )
                .expect("the lower tower has an authored parent");
            let platform = world::layer::cromatolis_aerial_citadel::cromatolis_aerial_citadel_lower_tower_platform_center(
                lower_tower_index,
            )
            .expect("the lower tower platform is authored in the world layer");
            assert_eq!(
                platform.xy(),
                upper_turret_center(parent_tower_index).map(|axis| axis as i32),
            );
            assert!(platform.z < upper_turret_pivot(parent_tower_index).z as i32);
        }
    }

    #[test]
    fn lower_safety_floor_sits_below_the_final_stair_and_cannon_platform() {
        let platform = world::layer::cromatolis_aerial_citadel::cromatolis_aerial_citadel_lower_tower_platform_center(0)
                .expect("the lower pilot platform is authored in the world layer");

        assert_eq!(
            lower_tower_safety_floor_pivot(0),
            Vec3::new(
                platform.x as f32,
                platform.y as f32,
                platform.z as f32 - 1.0
            ),
            "the energy floor must catch a fall beneath, not intersect, the final stair tread",
        );
    }

    #[test]
    fn lower_force_field_floor_has_a_solid_server_side_collision_plate_matching_the_authored_radius()
     {
        let features = super::aerial_features();
        let radius = features.lower_platform().horizontal_radius;
        let thickness = features.lower_safety_floor_thickness_m;

        let comp::Collider::CapsulePrism(collider) =
            lower_tower_force_field_collider(radius, thickness)
        else {
            panic!("the lower safety floor must use an invisible capsule-prism collider");
        };
        assert_eq!(collider.p0, Vec2::new(-0.5, 0.0));
        assert_eq!(collider.p1, Vec2::new(0.5, 0.0));
        assert_eq!(collider.radius, radius);
        assert_eq!(collider.z_min, 0.0);
        assert_eq!(collider.z_max, thickness);
        assert!(
            !lower_tower_force_field_collider(radius, thickness).is_voxel(),
            "the collision plate must never become a Voxygen-rendered voxel mesh",
        );
    }

    #[test]
    fn lower_stations_reverse_upper_weapons_and_mount_downward() {
        for lower_tower_index in 0
            ..world::layer::cromatolis_aerial_citadel::cromatolis_aerial_citadel_lower_tower_count()
        {
            let parent_tower_index = world::layer::cromatolis_aerial_citadel::cromatolis_aerial_citadel_lower_tower_upper_index(
                    lower_tower_index,
                )
                .expect("each lower station must have a parent tower");
            assert_eq!(
                lower_tower_turret_body(lower_tower_index),
                match upper_turret_body(parent_tower_index) {
                    Body::CitadelArcaneCannon => Body::CitadelArcaneSphereCannon,
                    Body::CitadelArcaneSphereCannon => Body::CitadelArcaneCannon,
                    _ => unreachable!("upper citadel stations have one of the two cannon bodies"),
                },
                "lower tower {lower_tower_index} must reverse its parent weapon",
            );
            let platform = world::layer::cromatolis_aerial_citadel::cromatolis_aerial_citadel_lower_tower_platform_center(
                lower_tower_index,
            )
            .expect("the lower cannon must have a structural platform");
            let pivot = lower_tower_turret_pivot(lower_tower_index);
            assert_eq!(pivot.xy(), platform.xy().map(|axis| axis as f32));
            let cannon = comp::Body::Object(lower_tower_turret_body(lower_tower_index));
            assert_eq!(
                pivot.z + cannon.height(),
                platform.z as f32,
                "the upright physical base must end exactly at the stone platform underside",
            );
            assert!(
                lower_tower_turret_rest_pose(lower_tower_index).pitch < 0.0,
                "negative local pitch must lower the barrel without inverting the base",
            );
            let world_barrel_direction =
                vek::Quaternion::rotation_x(lower_tower_turret_rest_pose(lower_tower_index).pitch)
                    * Vec3::unit_y();
            assert!(
                world_barrel_direction.z < -0.999,
                "the regular barrel joint must direct the lower cannon vertically down",
            );
        }
        assert_eq!(
            super::aerial_features().lower_tower_dome().shape,
            CitadelForceFieldShape::InvertedDome,
        );
    }

    #[test]
    fn lower_station_recovery_migrates_the_legacy_flipped_mount_without_selecting_other_cannons() {
        let pivot = lower_tower_turret_pivot(0);
        let expected_body = comp::Body::Object(Body::CitadelArcaneSphereCannon);
        let cannon = expected_body;

        assert!(lower_tower_turret_is_recovery_candidate(
            pivot,
            expected_body,
            cannon,
            pivot + Vec3::unit_z() * cannon.height(),
        ));
        assert!(!lower_tower_turret_is_recovery_candidate(
            pivot,
            expected_body,
            cannon,
            pivot + Vec3::unit_z() * 8.1,
        ));
        assert!(!lower_tower_turret_is_recovery_candidate(
            pivot,
            expected_body,
            comp::Body::Object(Body::CitadelArcaneCannon),
            pivot,
        ));
    }

    #[test]
    fn recovery_writes_only_missing_or_changed_components() {
        let current = super::aerial_features().upper_tower();
        assert!(
            !component_needs_restore(Some(&current), &super::aerial_features().upper_tower()),
            "a one-second recovery pass must not re-sync an unchanged cannon",
        );
        assert!(component_needs_restore(
            None::<&CitadelForceFieldVisual>,
            &super::aerial_features().upper_tower(),
        ));
        assert!(component_needs_restore(
            Some(&current),
            &super::aerial_features().lower_tower_dome(),
        ));
    }
}

/// Exercises `ensure_upper_defences`/`ensure_lower_tower_defences` against a
/// real `specs::World` (via `common_state::State`), the same pattern
/// `events::entity_manipulation`'s and `events::remote_sense`'s test modules
/// already use for server-crate integration tests. The property under test
/// -- a second reconciliation pass must never double-spawn an already-live
/// station -- was previously only verified by reading the code, not by a
/// test, even though it is exactly the guarantee the whole 1Hz refresh
/// depends on.
#[cfg(test)]
mod idempotency_tests {
    use super::{
        ensure_lower_tower_defences, ensure_upper_defences, lower_tower_force_field_pivot,
        upper_turret_pivot,
    };
    use common::{
        comp,
        resources::GameMode,
        terrain::{MapSizeLg, TerrainChunk, TerrainGrid},
    };
    use common_state::State;
    use specs::{Join, WorldExt};
    use std::sync::Arc;
    use vek::{Vec2, Vec3};

    const WORLD_CHUNKS_LG: MapSizeLg =
        if let Ok(map_size_lg) = MapSizeLg::new(Vec2 { x: 10, y: 10 }) {
            map_size_lg
        } else {
            panic!("Default world chunk size does not satisfy required invariants.");
        };

    fn setup() -> State {
        let pools = State::pools(GameMode::Server);
        let mut state = State::new(
            GameMode::Server,
            pools,
            WORLD_CHUNKS_LG,
            Arc::new(TerrainChunk::water(0)),
            |dispatch_builder| {
                common_systems::add_local_systems(dispatch_builder);
            },
            #[cfg(feature = "plugins")]
            common_state::plugin::PluginMgr::default(),
        );
        // `Anchor` is a server-only component, normally registered by
        // `Server::new` -- every station `spawn_upper_defence`/
        // `spawn_lower_tower_defence`/`ensure_lower_tower_force_field`/
        // `ensure_lower_tower_dome` creates carries one, so a bare `State`
        // needs it registered by hand here.
        state.ecs_mut().register::<comp::Anchor>();
        state
    }

    /// Loads a real chunk at the key covering `pos`, so `terrain_home_chunk`
    /// resolves it -- mirrors the "insert one real chunk into an otherwise
    /// empty `TerrainGrid`" pattern `events::remote_sense`'s `empty_terrain`
    /// helper already established. A tower's force-field/safety-floor/cannon
    /// pivots only ever differ in z (never x/y), and the terrain grid keys
    /// chunks purely by x/y, so one call here covers every co-located
    /// placement at that tower.
    fn load_chunk_containing(state: &mut State, pos: Vec3<f32>) {
        let key = state.terrain().pos_key(pos.map(|axis| axis.floor() as i32));
        state
            .ecs_mut()
            .write_resource::<TerrainGrid>()
            .insert(key, Arc::new(TerrainChunk::water(0)));
    }

    fn entity_count(state: &State) -> usize { state.ecs().entities().join().count() }

    #[test]
    fn ensure_upper_defences_never_double_spawns_an_already_live_station() {
        let mut state = setup();
        // Only tower 0's chunk is loaded, so exactly one station can ever
        // spawn here -- keeps the entity-count assertions unambiguous.
        load_chunk_containing(&mut state, upper_turret_pivot(0));
        let before = entity_count(&state);

        let first_pass_spawned = ensure_upper_defences(&mut state);
        assert_eq!(
            first_pass_spawned, 1,
            "exactly the one tower with a loaded chunk must spawn"
        );
        assert_eq!(
            entity_count(&state),
            before + 1,
            "the first pass must create exactly one new entity"
        );

        let second_pass_spawned = ensure_upper_defences(&mut state);
        assert_eq!(
            second_pass_spawned, 0,
            "a second reconciliation pass must not spawn a duplicate for an already-live station"
        );
        assert_eq!(
            entity_count(&state),
            before + 1,
            "entity count must not grow across an idempotent reconciliation pass"
        );
    }

    #[test]
    fn ensure_lower_tower_defences_never_double_spawns_an_already_live_station() {
        let mut state = setup();
        load_chunk_containing(&mut state, lower_tower_force_field_pivot(0));
        let before = entity_count(&state);

        let first_pass_changes = ensure_lower_tower_defences(&mut state);
        assert_eq!(
            first_pass_changes, 2,
            "one change from the safety floor and one from the cannon+dome pair on tower 0; every \
             other tower's chunk is unloaded and reports no change"
        );
        assert_eq!(
            entity_count(&state),
            before + 3,
            "the first pass must create exactly three new entities: the safety floor, the cannon, \
             and the dome"
        );

        let second_pass_changes = ensure_lower_tower_defences(&mut state);
        assert_eq!(
            second_pass_changes, 0,
            "a second reconciliation pass must not report any change once every lower-tower \
             entity already matches its desired state"
        );
        assert_eq!(
            entity_count(&state),
            before + 3,
            "entity count must not grow across an idempotent reconciliation pass"
        );
    }
}
