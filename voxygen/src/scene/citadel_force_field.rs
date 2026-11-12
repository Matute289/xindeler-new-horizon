//! Client-only presentation for hollow Cromatolis Aerial Citadel force
//! fields.
//!
//! The terrain sprite renderer has no per-voxel alpha channel, so a glass
//! block is necessarily opaque. This mesh is drawn in the existing transparent
//! trail pass instead: it is visible from both sides, leaves its volume empty,
//! and keeps the field's future collision rules out of rendering.

use super::SceneData;
use crate::render::{DynamicModel, Mesh, Quad, Renderer, TrailDrawer, TrailVertex};
use common::comp::{CitadelForceFieldShape, CitadelForceFieldVisual, Pos};
use specs::{Join, WorldExt};
use vek::Vec3;

const AZIMUTH_SEGMENTS: usize = 24;
const ELEVATION_SEGMENTS: usize = 12;
/// A few very close skins make the low-alpha transparent pass legible while
/// remaining one continuous field rather than opaque laminated terrain.
const FIELD_SKINS: usize = 3;
const FIELD_SKIN_SPACING: f32 = 0.035;
// `TrailVertex` uses the renderer's shared u16 quad-index buffer (see
// `render::pipelines::trail::Vertex::QUADS_INDEX`). Keep each uploaded model
// four-vertex aligned and strictly below that index limit. With 48 domes
// (24 upper `Dome` + 24 lower `InvertedDome`, one per tower) across the full
// citadel ring and `AZIMUTH_SEGMENTS * ELEVATION_SEGMENTS * FIELD_SKINS * 4`
// vertices per dome, this batches the ring into exactly 3 draws -- see
// `full_citadel_force_field_ring_is_split_before_the_u16_trail_index_limit`
// below, which locks that batch count in against a regression.
const MAX_TRAIL_BATCH_VERTICES: usize = (u16::MAX as usize / 4) * 4;

struct ForceFieldBatch {
    dynamic_model: DynamicModel<TrailVertex>,
    model_len: u32,
}

/// The complete render-relevant state of one force field. Keeping this small
/// snapshot lets the transparent mesh remain on the GPU during static frames.
#[derive(Clone, Copy, Debug, PartialEq)]
struct ForceFieldInstance {
    pivot: Vec3<f32>,
    visual: CitadelForceFieldVisual,
}

fn force_field_layout_changed(
    cached: &[ForceFieldInstance],
    current: &[ForceFieldInstance],
) -> bool {
    cached != current
}

// Citadel-visual client builds (this module, the turret animation, and the
// new `ParticleMode` variants) and citadel-aware server builds (station
// placement in `server::citadel`, already shipped) are released together per
// this repo's split client/server release pipelines -- a client without this
// code can still connect to a citadel-aware server, but will render nothing
// for its stations/force fields until it upgrades.
#[derive(Default)]
pub struct CitadelForceFieldMgr {
    batches: Vec<ForceFieldBatch>,
    /// Last layout uploaded to the trail pipeline. The fields are normally
    /// static; entity spawn/despawn, visual edits, or position/interpolation
    /// changes invalidate this snapshot and rebuild the mesh.
    layout: Vec<ForceFieldInstance>,
}

impl CitadelForceFieldMgr {
    pub fn maintain(&mut self, renderer: &mut Renderer, scene_data: &SceneData) {
        let ecs = scene_data.state.ecs();
        let positions = ecs.read_storage::<Pos>();
        let force_fields = ecs.read_storage::<CitadelForceFieldVisual>();
        // Fields are mounted to authored, immovable stations. Use their
        // authoritative ECS positions rather than frontend interpolation: a
        // transient interpolation update must never make a static dome appear
        // to shiver or invalidate the cached mesh.
        let layout = (&positions, &force_fields)
            .join()
            .map(|(position, force_field)| ForceFieldInstance {
                pivot: position.0,
                visual: *force_field,
            })
            .collect::<Vec<_>>();

        if !force_field_layout_changed(&self.layout, &layout) {
            return;
        }

        let mut meshes = Vec::new();
        for field in &layout {
            append_force_field_to_batches(&mut meshes, field.pivot, field.visual);
        }

        if meshes.is_empty() {
            self.batches.clear();
            self.layout = layout;
            return;
        }

        self.batches.truncate(meshes.len());
        for (index, mesh) in meshes.into_iter().enumerate() {
            let recreate = self
                .batches
                .get(index)
                .is_none_or(|batch| batch.dynamic_model.len() < mesh.len());
            if recreate {
                let batch = ForceFieldBatch {
                    dynamic_model: renderer.create_dynamic_model(mesh.len()),
                    model_len: 0,
                };
                if index == self.batches.len() {
                    self.batches.push(batch);
                } else {
                    self.batches[index] = batch;
                }
            }
            let batch = &mut self.batches[index];
            renderer.update_model(&batch.dynamic_model, &mesh, 0);
            batch.model_len = mesh.len() as u32;
        }
        self.layout = layout;
    }

    pub fn render<'a>(&'a self, drawer: &mut TrailDrawer<'_, 'a>) {
        for batch in &self.batches {
            if batch.model_len > 0 {
                drawer.draw(batch.dynamic_model.submodel(0..batch.model_len));
            }
        }
    }
}

fn append_force_field_to_batches(
    batches: &mut Vec<Mesh<TrailVertex>>,
    pivot: Vec3<f32>,
    force_field: CitadelForceFieldVisual,
) {
    let mut field_mesh = Mesh::new();
    append_force_field_mesh(&mut field_mesh, pivot, force_field);
    if field_mesh.is_empty() {
        return;
    }
    assert!(
        field_mesh.len() <= MAX_TRAIL_BATCH_VERTICES,
        "a single citadel force field must fit the u16 trail index buffer"
    );

    if batches
        .last()
        .is_none_or(|batch| batch.len() + field_mesh.len() > MAX_TRAIL_BATCH_VERTICES)
    {
        batches.push(field_mesh);
    } else if let Some(batch) = batches.last_mut() {
        batch.push_mesh(&field_mesh);
    }
}

fn append_force_field_mesh(
    mesh: &mut Mesh<TrailVertex>,
    pivot: Vec3<f32>,
    force_field: CitadelForceFieldVisual,
) {
    // `SafetyFloor` exists solely as server-side collision (see
    // `server::citadel::lower_tower_force_field_collider`); the visible
    // floor is the authored stone mounting platform, not a second opaque
    // surface, so it has no client mesh at all. This repo's
    // `CitadelForceFieldShape` has no separate `Floor` variant (unlike the
    // reference implementation's four-variant enum), so there is no ring-mesh
    // branch to port here.
    if force_field.shape == CitadelForceFieldShape::SafetyFloor {
        return;
    }
    if !force_field.horizontal_radius.is_finite() || force_field.horizontal_radius <= 0.0 {
        return;
    }

    let vertex = |point: Vec3<f32>| TrailVertex {
        pos: point.into_array(),
    };
    for skin in 0..FIELD_SKINS {
        let offset = (skin as f32 - (FIELD_SKINS - 1) as f32 * 0.5) * FIELD_SKIN_SPACING;
        let radius = force_field.horizontal_radius + offset;
        if radius <= 0.0 {
            continue;
        }

        let height = force_field.height + offset;
        if !height.is_finite() || height <= 0.0 {
            continue;
        }
        let vertical_direction = match force_field.shape {
            CitadelForceFieldShape::Dome => 1.0,
            CitadelForceFieldShape::InvertedDome => -1.0,
            CitadelForceFieldShape::SafetyFloor => {
                unreachable!("the SafetyFloor case returns above")
            },
        };

        for elevation in 0..ELEVATION_SEGMENTS {
            let lower = elevation as f32 / ELEVATION_SEGMENTS as f32 * std::f32::consts::FRAC_PI_2;
            let upper =
                (elevation + 1) as f32 / ELEVATION_SEGMENTS as f32 * std::f32::consts::FRAC_PI_2;
            for azimuth in 0..AZIMUTH_SEGMENTS {
                let start = azimuth as f32 / AZIMUTH_SEGMENTS as f32 * std::f32::consts::TAU;
                let end = (azimuth + 1) as f32 / AZIMUTH_SEGMENTS as f32 * std::f32::consts::TAU;
                let point = |angle: f32, elevation: f32| {
                    pivot
                        + Vec3::new(
                            radius * elevation.cos() * angle.cos(),
                            radius * elevation.cos() * angle.sin(),
                            vertical_direction * height * elevation.sin(),
                        )
                };
                mesh.push_quad(Quad::new(
                    vertex(point(start, lower)),
                    vertex(point(start, upper)),
                    vertex(point(end, upper)),
                    vertex(point(end, lower)),
                ));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{
        AZIMUTH_SEGMENTS, ELEVATION_SEGMENTS, FIELD_SKINS, ForceFieldInstance,
        MAX_TRAIL_BATCH_VERTICES, append_force_field_mesh, append_force_field_to_batches,
        force_field_layout_changed,
    };
    use crate::render::{Mesh, TrailVertex};
    use common::comp::{CitadelForceFieldShape, CitadelForceFieldVisual};
    use vek::Vec3;

    fn upper_tower_visual() -> CitadelForceFieldVisual {
        CitadelForceFieldVisual {
            shape: CitadelForceFieldShape::Dome,
            horizontal_radius: 14.0,
            height: 20.0,
        }
    }

    fn lower_platform_visual() -> CitadelForceFieldVisual {
        CitadelForceFieldVisual {
            shape: CitadelForceFieldShape::SafetyFloor,
            horizontal_radius: 10.0,
            height: 0.0,
        }
    }

    fn lower_tower_dome_visual() -> CitadelForceFieldVisual {
        CitadelForceFieldVisual {
            shape: CitadelForceFieldShape::InvertedDome,
            horizontal_radius: 12.0,
            height: 18.0,
        }
    }

    #[test]
    fn unchanged_force_field_layout_skips_mesh_refresh() {
        let field = ForceFieldInstance {
            pivot: Vec3::new(100.0, 200.0, 300.0),
            visual: upper_tower_visual(),
        };
        let cached = vec![field];

        assert!(
            !force_field_layout_changed(&cached, &cached),
            "an identical static layout must keep its GPU mesh",
        );
        assert!(force_field_layout_changed(&cached, &[ForceFieldInstance {
            pivot: Vec3::new(101.0, 200.0, 300.0),
            ..field
        }],));
        assert!(force_field_layout_changed(&cached, &[ForceFieldInstance {
            visual: lower_tower_dome_visual(),
            ..field
        }],));
        assert!(force_field_layout_changed(&cached, &[]));
    }

    #[test]
    fn force_field_mesh_is_a_full_hollow_semi_bubble() {
        let mut mesh = Mesh::<TrailVertex>::new();
        append_force_field_mesh(
            &mut mesh,
            Vec3::new(100.0, 200.0, 300.0),
            upper_tower_visual(),
        );

        assert_eq!(
            mesh.len(),
            AZIMUTH_SEGMENTS * ELEVATION_SEGMENTS * FIELD_SKINS * 4,
        );
        assert!(
            mesh.vertices().iter().all(|vertex| vertex.pos[2] >= 300.0),
            "a force field must never fill below its tower-deck pivot",
        );
    }

    #[test]
    fn lower_safety_floor_does_not_cover_the_stone_mounting_platform() {
        let mut mesh = Mesh::<TrailVertex>::new();
        append_force_field_mesh(
            &mut mesh,
            Vec3::new(100.0, 200.0, 300.0),
            lower_platform_visual(),
        );

        assert_eq!(mesh.len(), 0);
        assert_eq!(
            lower_platform_visual().shape,
            CitadelForceFieldShape::SafetyFloor,
        );
    }

    #[test]
    fn lower_tower_dome_opens_below_its_cannon_platform() {
        let mut mesh = Mesh::<TrailVertex>::new();
        append_force_field_mesh(
            &mut mesh,
            Vec3::new(100.0, 200.0, 300.0),
            lower_tower_dome_visual(),
        );

        assert_eq!(
            mesh.len(),
            AZIMUTH_SEGMENTS * ELEVATION_SEGMENTS * FIELD_SKINS * 4,
        );
        assert!(
            mesh.vertices().iter().all(|vertex| vertex.pos[2] <= 300.0),
            "an under-island cannon field must extend downward from its platform",
        );
        assert_eq!(
            lower_tower_dome_visual().shape,
            CitadelForceFieldShape::InvertedDome,
        );
    }

    #[test]
    fn full_citadel_force_field_ring_is_split_before_the_u16_trail_index_limit() {
        let mut batches = Vec::new();
        for tower in 0..24 {
            append_force_field_to_batches(
                &mut batches,
                Vec3::new(tower as f32 * 20.0, 0.0, 0.0),
                upper_tower_visual(),
            );
        }
        for tower in 0..24 {
            append_force_field_to_batches(
                &mut batches,
                Vec3::new(tower as f32 * 20.0, 40.0, -18.0),
                lower_tower_dome_visual(),
            );
        }

        assert_eq!(
            batches.len(),
            3,
            "48 domes (24 upper + 24 lower) must batch into exactly 3 draws",
        );
        assert!(
            batches
                .iter()
                .all(|batch| batch.len() <= MAX_TRAIL_BATCH_VERTICES),
            "each transparent draw must fit the trail pipeline's u16 indices",
        );
        assert_eq!(
            batches.iter().map(Mesh::len).sum::<usize>(),
            24 * AZIMUTH_SEGMENTS * ELEVATION_SEGMENTS * FIELD_SKINS * 4
                + 24 * AZIMUTH_SEGMENTS * ELEVATION_SEGMENTS * FIELD_SKINS * 4,
        );
    }
}
