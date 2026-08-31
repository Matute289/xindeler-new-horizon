//! `portrait_gen` — headless renderer that turns a character's persisted body
//! and inventory into a portrait image.
//!
//! A pure, stateless CLI filter: it reads one JSON [`PortraitRequest`] on
//! stdin, renders the described character with the same CPU-only voxel
//! rasterizer the UI icons use (`ui::graphic::renderer::draw_voxes`, backed by
//! `euc` — no GPU, no window, no wgpu), and writes the encoded image bytes to
//! stdout. It knows nothing about databases, HTTP, or the game server; callers
//! run it as a short-lived subprocess.
//!
//! Build:
//!
//! ```text
//! cargo build -p xindeler-voxygen --bin portrait_gen
//! ```
//!
//! Run (assets must be reachable, as for every asset-loading binary):
//!
//! ```text
//! VELOREN_ASSETS="$(pwd)/assets" portrait_gen < request.json > portrait.webp
//! ```
//!
//! Exit codes, so a caller can distinguish a caller bug from a renderer bug:
//!
//! | code | meaning                                                        |
//! |------|----------------------------------------------------------------|
//! | 0    | success — stdout holds the encoded image                       |
//! | 2    | the request could not be parsed or its params are out of range |
//! | 3    | the body kind is not supported (humanoids only)                |
//! | 4    | any other failure (assets, meshing, encoding)                   |
//!
//! Diagnostics go to stderr only; stdout carries image bytes and nothing else.

use std::{
    io::{Read, Write},
    process::ExitCode,
    sync::Arc,
};

use common::{
    comp::{
        self, Body, CharacterState, Inventory,
        item::{ItemKind, armor::ArmorKind},
        slot::{ArmorSlot, EquipSlot},
    },
    figure::Segment,
    resources::Time,
    util::Dir,
    vol::{IntoFullVolIterator, SizedVol},
};
use image::{ImageEncoder, RgbaImage, codecs::webp::WebPEncoder};
use serde::{Deserialize, Serialize};
use vek::{Mat4, Quaternion, Vec2, Vec3};

use anim::{Animation, FigureBoneData, Skeleton, character::CharacterSkeleton};
use xindeler_voxygen::{
    scene::{
        CameraMode,
        figure::{
            cache::{CharacterCacheKey, FigureKey},
            load::BodySpec,
        },
    },
    ui::{SampleStrat, Transform, graphic::renderer::draw_voxes},
};

// ---------------------------------------------------------------------------
// Protocol
// ---------------------------------------------------------------------------

/// Renderer/params version, echoed through the request so a caller can prove
/// the renderer it spawned agrees with the version its cache keys were built
/// from. Bumping it is how a purely visual change invalidates cached output.
pub const PORTRAIT_PARAMS_VERSION: &str = "p1";

/// Output edge length, in pixels, when the request does not say.
pub const DEFAULT_PORTRAIT_SIZE: u16 = 256;

/// Accepted range for [`PortraitParams::size`]. The upper bound keeps the
/// supersampled intermediate buffers bounded (a 512² request rasterizes at
/// 1024², ~4 MB per buffer) — anything larger is a malformed request, not a
/// portrait.
pub const MIN_PORTRAIT_SIZE: u16 = 16;
pub const MAX_PORTRAIT_SIZE: u16 = 512;

/// How the posed model is framed inside the output square.
///
/// One preset today; it is an enum rather than a flag so that adding a second
/// framing is an additive change to the wire format.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Framing {
    /// Whole figure seen straight-on, weapons omitted, background left
    /// transparent so the consumer decides what sits behind it.
    #[default]
    FullBodyFront,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PortraitParams {
    /// Edge length of the (square) output image, in pixels.
    #[serde(default = "default_size")]
    pub size: u16,
    #[serde(default)]
    pub framing: Framing,
    /// See [`PORTRAIT_PARAMS_VERSION`].
    #[serde(default = "default_version")]
    pub version: String,
}

fn default_size() -> u16 { DEFAULT_PORTRAIT_SIZE }

fn default_version() -> String { PORTRAIT_PARAMS_VERSION.to_string() }

impl Default for PortraitParams {
    fn default() -> Self {
        Self {
            size: default_size(),
            framing: Framing::default(),
            version: default_version(),
        }
    }
}

/// The whole of stdin: everything the renderer needs, and nothing it could
/// look up for itself.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PortraitRequest {
    pub body: Body,
    pub inventory: Inventory,
    #[serde(default)]
    pub params: PortraitParams,
}

// ---------------------------------------------------------------------------
// Framing presets
// ---------------------------------------------------------------------------

/// Whether a framing preset draws the equipped mainhand/offhand weapons. The
/// idle pose *can* hold them; a portrait that shows armour only does not.
const fn framing_shows_weapons(framing: Framing) -> bool {
    match framing {
        Framing::FullBodyFront => false,
    }
}

/// Fraction of the frame the figure fills along its longest visible axis. The
/// remainder is the margin that keeps boots and pointed hats off the edge.
const FRAME_FILL: f32 = 0.94;

/// Camera orientation for a framing preset.
fn framing_ori(framing: Framing) -> Quaternion<f32> {
    match framing {
        // Stand the model upright (turn the model's +Z up onto screen-up) and
        // then turn it around to face the camera.
        Framing::FullBodyFront => Quaternion::rotation_x(-90.0 * std::f32::consts::PI / 180.0)
            .rotated_y(180.0 * std::f32::consts::PI / 180.0),
    }
}

/// The extent `draw_voxes` derives its projection scale from.
///
/// It only takes two corners of each bone's volume, so this is not a true
/// bounding box — but it is the number the unstretched projection divides by,
/// and [`fit`] has to compensate for exactly that number to place the figure.
fn projection_reference_extent(bones: &[(Mat4<f32>, &Segment)]) -> f32 {
    let mut min = Vec3::broadcast(f32::INFINITY);
    let mut max = Vec3::broadcast(f32::NEG_INFINITY);
    for (bone, segment) in bones {
        let volume = segment.size().as_::<f32>();
        for corner in [bone.mul_point(Vec3::zero()), bone.mul_point(volume)] {
            min = min.map2(corner, f32::min);
            max = max.map2(corner, f32::max);
        }
    }
    let size = max - min;
    size.x.max(size.y).max(size.z)
}

/// Axis-aligned bounds, in the rotated (screen-facing) space, of the voxels
/// that will actually be drawn.
///
/// Empty padding inside a bone's volume — the slack around a quiver, say — is
/// excluded, so the figure ends up framed on itself rather than on whatever
/// its loosest bone volume happens to be. Returns `None` when nothing is
/// visible at all.
fn drawn_bounds(bones: &[(Mat4<f32>, &Segment)], ori: Mat4<f32>) -> Option<(Vec3<f32>, Vec3<f32>)> {
    let mut min = Vec3::broadcast(f32::INFINITY);
    let mut max = Vec3::broadcast(f32::NEG_INFINITY);
    let mut any = false;

    for (bone, segment) in bones {
        let to_screen = ori * *bone;

        let mut lo = Vec3::broadcast(i32::MAX);
        let mut hi = Vec3::broadcast(i32::MIN);
        let mut filled = false;
        for (pos, vox) in segment.full_vol_iter() {
            // `get_color` is exactly the test the mesher uses to decide whether
            // a voxel contributes geometry.
            if vox.get_color().is_some() {
                filled = true;
                lo = lo.map2(pos, i32::min);
                hi = hi.map2(pos, i32::max);
            }
        }
        if !filled {
            continue;
        }
        any = true;

        // The mesher emits a unit cube per filled voxel, so the drawn extent
        // runs one voxel past the last voxel's origin.
        let lo = lo.as_::<f32>();
        let hi = hi.as_::<f32>() + Vec3::one();
        for corner in 0..8u8 {
            let p = to_screen.mul_point(Vec3::new(
                if corner & 1 == 0 { lo.x } else { hi.x },
                if corner & 2 == 0 { lo.y } else { hi.y },
                if corner & 4 == 0 { lo.z } else { hi.z },
            ));
            min = min.map2(p, f32::min);
            max = max.map2(p, f32::max);
        }
    }

    any.then_some((min, max))
}

/// Builds the [`Transform`] that centres the drawn figure in the frame and
/// scales it to fill [`FRAME_FILL`] of the shorter dimension.
///
/// `draw_voxes`' own centring translation is disabled (the returned
/// `offset_scaling` is zero) because it subtracts half the bounding box's
/// *size* rather than its centre, which leaves an off-centre figure for any
/// model whose bounds do not start at the origin — every posed character.
fn fit(
    framing: Framing,
    bones: &[(Mat4<f32>, &Segment)],
) -> Result<(Transform, Vec3<f32>), PortraitError> {
    let ori = framing_ori(framing);
    let ori_mat = Mat4::from(ori);

    let (min, max) = drawn_bounds(bones, ori_mat).ok_or_else(|| {
        PortraitError::Failed("the posed body contains no visible voxels".to_string())
    })?;
    let size = max - min;
    let centre = (min + max) * 0.5;

    let reference = projection_reference_extent(bones);
    let visible = size.x.max(size.y);
    if !(reference.is_finite() && reference > 0.0) || !(visible.is_finite() && visible > 0.0) {
        return Err(PortraitError::Failed(
            "the posed body has a degenerate bounding box".to_string(),
        ));
    }

    let zoom = FRAME_FILL * reference / visible;
    // `draw_voxes` scales the rotated model by this factor before the
    // orthographic projection, whose depth range is 0..1.
    let scale = 2.0 * zoom / reference;

    Ok((
        Transform {
            ori,
            // x/y centre the figure; z parks the depth range around the middle
            // of the near/far span so no part of the model is depth-clipped.
            offset: Vec3::new(-centre.x, -centre.y, 0.5 / scale - centre.z),
            zoom,
            orth: true,
            stretch: false,
        },
        Vec3::zero(),
    ))
}

// ---------------------------------------------------------------------------
// Errors
// ---------------------------------------------------------------------------

#[derive(Debug)]
enum PortraitError {
    /// Malformed or out-of-range request.
    BadRequest(String),
    /// A body kind this renderer does not draw.
    UnsupportedBody(String),
    /// Assets, meshing or encoding failed.
    Failed(String),
}

impl PortraitError {
    /// The process exit code this error is reported as. See the module docs.
    fn exit_code(&self) -> u8 {
        match self {
            PortraitError::BadRequest(_) => 2,
            PortraitError::UnsupportedBody(_) => 3,
            PortraitError::Failed(_) => 4,
        }
    }
}

impl std::fmt::Display for PortraitError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PortraitError::BadRequest(msg) => write!(f, "bad request: {msg}"),
            PortraitError::UnsupportedBody(msg) => write!(f, "unsupported body: {msg}"),
            PortraitError::Failed(msg) => write!(f, "render failed: {msg}"),
        }
    }
}

// ---------------------------------------------------------------------------
// Rendering
// ---------------------------------------------------------------------------

/// Rejects requests whose params fall outside what the renderer will honour.
fn validate(params: &PortraitParams) -> Result<(), PortraitError> {
    if !(MIN_PORTRAIT_SIZE..=MAX_PORTRAIT_SIZE).contains(&params.size) {
        return Err(PortraitError::BadRequest(format!(
            "size {} outside {MIN_PORTRAIT_SIZE}..={MAX_PORTRAIT_SIZE}",
            params.size
        )));
    }
    Ok(())
}

/// Drops the weapons from a copy of the inventory, so that neither the model
/// cache key nor the idle animation sees them: the two weapon bones are
/// derived purely from the cache key's tool entries, so an empty pair means no
/// weapon geometry at all, and the stand animation then poses empty hands.
fn strip_weapons(inventory: &mut Inventory) {
    let time = Time(0.0);
    for slot in [
        EquipSlot::ActiveMainhand,
        EquipSlot::ActiveOffhand,
        EquipSlot::InactiveMainhand,
        EquipSlot::InactiveOffhand,
    ] {
        // The removed item is deliberately dropped: this inventory is a
        // throwaway render input that is never persisted.
        let _ = inventory.replace_loadout_item(slot, None, time);
    }
}

/// Poses a humanoid wearing `inventory` and returns the per-bone
/// `(transform, segment)` pairs `draw_voxes` consumes.
fn humanoid_bones(
    body: comp::humanoid::Body,
    inventory: &Inventory,
    manifest: &<comp::humanoid::Body as BodySpec>::Manifests,
) -> Vec<(Mat4<f32>, Segment)> {
    let state = CharacterState::Idle(Default::default());
    let extra = Some(Arc::new(CharacterCacheKey::from(
        Some(&state),
        CameraMode::ThirdPerson,
        inventory,
    )));

    let bone_segments = comp::humanoid::Body::bone_meshes(
        &FigureKey {
            body,
            item_key: None,
            extra,
        },
        manifest,
        (),
    );

    // A backpack or cloak shifts the idle pose's shoulders, so a geared
    // character is not posed as a bare one.
    let back_carry_offset = inventory
        .equipped(EquipSlot::Armor(ArmorSlot::Back))
        .and_then(|i| {
            if let ItemKind::Armor(armor) = i.kind().as_ref() {
                match &armor.kind {
                    ArmorKind::Backpack => Some(4.0),
                    ArmorKind::Back => Some(1.5),
                    _ => None,
                }
            } else {
                None
            }
        })
        .unwrap_or(0.0);

    let mut buf = [FigureBoneData::default(); 16];
    let skel = anim::character::StandAnimation::update_skeleton(
        &CharacterSkeleton::new(false, back_carry_offset, 1.0),
        (
            // No tool kinds and no hands: the portrait shows armour only, so
            // the pose must not reach for a weapon that will not be drawn.
            None,
            None,
            (None, None),
            anim::vek::Vec3::<f32>::unit_y(),
            anim::vek::Vec3::<f32>::unit_y(),
            Dir::new(Vec3::unit_y()),
            0.0,
            Vec3::zero(),
        ),
        0.0,
        &mut 1.0,
        &anim::character::SkeletonAttr::from(&body),
    );
    let _ = skel.compute_matrices(Mat4::identity(), &mut buf, body);

    bone_segments
        .into_iter()
        .zip(buf)
        .filter_map(|(segment, bone)| {
            let (segment, offset) = segment?;
            Some((
                Mat4::from_col_arrays(bone.0) * Mat4::translation_3d(offset),
                segment,
            ))
        })
        .collect()
}

/// Renders `request` to an in-memory RGBA image on a transparent background.
fn render(request: &PortraitRequest) -> Result<RgbaImage, PortraitError> {
    validate(&request.params)?;

    let body = match request.body {
        Body::Humanoid(body) => body,
        other => {
            return Err(PortraitError::UnsupportedBody(format!(
                "only humanoid characters can be rendered, got {other:?}"
            )));
        },
    };

    let mut inventory = request.inventory.clone();
    if !framing_shows_weapons(request.params.framing) {
        strip_weapons(&mut inventory);
    }

    let manifest = <comp::humanoid::Body as BodySpec>::load_spec()
        .map_err(|e| PortraitError::Failed(format!("could not load humanoid manifests: {e}")))?;

    let bones = humanoid_bones(body, &inventory, &manifest);
    if bones.is_empty() {
        return Err(PortraitError::Failed(
            "the posed body produced no drawable bones".to_string(),
        ));
    }
    let bones: Vec<_> = bones.iter().map(|(t, s)| (*t, s)).collect();

    let (transform, offset_scaling) = fit(request.params.framing, &bones)?;

    Ok(draw_voxes(
        &bones,
        Vec2::new(request.params.size, request.params.size),
        transform,
        SampleStrat::SuperSampling(4),
        offset_scaling,
    ))
}

/// Encodes an RGBA image as lossless WebP — the only mode this encoder offers,
/// and the right one for flat-shaded voxel art with a transparent background.
fn encode_webp(img: &RgbaImage) -> Result<Vec<u8>, PortraitError> {
    let mut out = Vec::new();
    WebPEncoder::new_lossless(&mut out)
        .write_image(
            img.as_raw(),
            img.width(),
            img.height(),
            image::ExtendedColorType::Rgba8,
        )
        .map_err(|e| PortraitError::Failed(format!("webp encode failed: {e}")))?;
    Ok(out)
}

fn run() -> Result<Vec<u8>, PortraitError> {
    let mut input = String::new();
    std::io::stdin()
        .read_to_string(&mut input)
        .map_err(|e| PortraitError::BadRequest(format!("could not read stdin: {e}")))?;

    let request: PortraitRequest = serde_json::from_str(&input)
        .map_err(|e| PortraitError::BadRequest(format!("could not parse request: {e}")))?;

    encode_webp(&render(&request)?)
}

fn main() -> ExitCode {
    match run() {
        Ok(bytes) => {
            let mut stdout = std::io::stdout().lock();
            if let Err(e) = stdout.write_all(&bytes).and_then(|()| stdout.flush()) {
                eprintln!("portrait_gen: could not write image to stdout: {e}");
                return ExitCode::from(4);
            }
            ExitCode::SUCCESS
        },
        Err(e) => {
            eprintln!("portrait_gen: {e}");
            ExitCode::from(e.exit_code())
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A fixed humanoid, so failures are reproducible rather than seed-shaped.
    fn test_body() -> comp::humanoid::Body {
        comp::humanoid::Body::iter()
            .next()
            .expect("there is at least one humanoid body")
    }

    #[test]
    fn request_json_round_trips() {
        let request = PortraitRequest {
            body: Body::Humanoid(test_body()),
            inventory: Inventory::with_empty(),
            params: PortraitParams::default(),
        };

        let json = serde_json::to_string(&request).expect("request serializes");
        let back: PortraitRequest = serde_json::from_str(&json).expect("request deserializes");

        assert_eq!(back.params, request.params);
        assert_eq!(back.body, request.body);
        assert_eq!(
            serde_json::to_string(&back.inventory).unwrap(),
            serde_json::to_string(&request.inventory).unwrap()
        );
    }

    #[test]
    fn params_default_when_absent() {
        let body = serde_json::to_string(&Body::Humanoid(test_body())).unwrap();
        let inventory = serde_json::to_string(&Inventory::with_empty()).unwrap();
        let json = format!(r#"{{"body":{body},"inventory":{inventory}}}"#);

        let request: PortraitRequest =
            serde_json::from_str(&json).expect("params are optional in the wire format");

        assert_eq!(request.params, PortraitParams::default());
        assert_eq!(request.params.size, DEFAULT_PORTRAIT_SIZE);
        assert_eq!(request.params.version, PORTRAIT_PARAMS_VERSION);
        assert_eq!(request.params.framing, Framing::FullBodyFront);
    }

    #[test]
    fn malformed_request_is_a_bad_request() {
        let err = serde_json::from_str::<PortraitRequest>("{ not json }")
            .map_err(|e| PortraitError::BadRequest(e.to_string()))
            .expect_err("malformed JSON must not parse");
        assert_eq!(err.exit_code(), 2);
    }

    #[test]
    fn out_of_range_size_is_a_bad_request() {
        for size in [0, MIN_PORTRAIT_SIZE - 1, MAX_PORTRAIT_SIZE + 1] {
            let params = PortraitParams {
                size,
                ..Default::default()
            };
            let err = validate(&params).expect_err("size must be range-checked");
            assert_eq!(err.exit_code(), 2, "size {size} should be rejected");
        }
        for size in [MIN_PORTRAIT_SIZE, DEFAULT_PORTRAIT_SIZE, MAX_PORTRAIT_SIZE] {
            let params = PortraitParams {
                size,
                ..Default::default()
            };
            validate(&params).expect("in-range sizes are accepted");
        }
    }

    #[test]
    fn non_humanoid_body_is_unsupported() {
        let request = PortraitRequest {
            body: Body::Object(comp::object::Body::Arrow),
            inventory: Inventory::with_empty(),
            params: PortraitParams::default(),
        };
        let err = render(&request).expect_err("only humanoids render");
        assert_eq!(err.exit_code(), 3);
    }

    /// Needs `VELOREN_ASSETS`, like every asset-loading test in the workspace.
    ///
    /// Pixel-exact goldens are too brittle across float architectures, so this
    /// asserts the properties that actually catch a broken pipeline: the right
    /// dimensions, something was drawn, the background stayed transparent, and
    /// the result is not one flat colour.
    #[test]
    fn renders_a_bare_humanoid() {
        let request = PortraitRequest {
            body: Body::Humanoid(test_body()),
            inventory: Inventory::with_empty(),
            params: PortraitParams {
                size: 64,
                ..Default::default()
            },
        };

        let img = render(&request).expect("a bare humanoid renders");
        assert_eq!((img.width(), img.height()), (64, 64));

        let opaque = img.pixels().filter(|p| p.0[3] > 0).count();
        assert!(opaque > 0, "the portrait is entirely transparent");
        assert!(
            opaque < (64 * 64),
            "the background should stay transparent, not be filled"
        );

        let first = img.pixels().find(|p| p.0[3] > 0).expect("an opaque pixel");
        assert!(
            img.pixels().any(|p| p.0[3] > 0 && p.0 != first.0),
            "the portrait is a single flat colour — nothing was shaded"
        );

        let bytes = encode_webp(&img).expect("the render encodes as lossless webp");
        assert!(!bytes.is_empty());
        assert_eq!(&bytes[0..4], b"RIFF");
        assert_eq!(&bytes[8..12], b"WEBP");
    }
}
