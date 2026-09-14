use super::super::{ExperimentalShader, GlobalsLayouts, PipelineModes, Vertex as VertexTrait};
use bytemuck::{Pod, Zeroable};
use std::mem;
use vek::*;

#[repr(C)]
#[derive(Copy, Clone, Debug, Zeroable, Pod)]
pub struct Vertex {
    pub pos: [f32; 3],
    // ____BBBBBBBBGGGGGGGGRRRRRRRR
    // col: u32 = "v_col",
    // ...AANNN
    // A = AO
    // N = Normal
    norm_ao: u32,
}

impl Vertex {
    pub fn new(pos: Vec3<f32>, norm: Vec3<f32>) -> Self {
        #[expect(clippy::bool_to_int_with_if)]
        let norm_bits = if norm.x != 0.0 {
            if norm.x < 0.0 { 0 } else { 1 }
        } else if norm.y != 0.0 {
            if norm.y < 0.0 { 2 } else { 3 }
        } else if norm.z < 0.0 {
            4
        } else {
            5
        };

        Self {
            pos: pos.into_array(),
            norm_ao: norm_bits,
        }
    }

    fn desc<'a>() -> wgpu::VertexBufferLayout<'a> {
        const ATTRIBUTES: [wgpu::VertexAttribute; 2] =
            wgpu::vertex_attr_array![0 => Float32x3, 1 => Uint32];
        wgpu::VertexBufferLayout {
            array_stride: Self::STRIDE,
            step_mode: wgpu::VertexStepMode::Vertex,
            attributes: &ATTRIBUTES,
        }
    }
}

impl VertexTrait for Vertex {
    const QUADS_INDEX: Option<wgpu::IndexFormat> = Some(wgpu::IndexFormat::Uint16);
    const STRIDE: wgpu::BufferAddress = mem::size_of::<Self>() as wgpu::BufferAddress;
}

/// The look and motion of a particle is selected entirely by this mode: it is
/// uploaded per instance as `inst_mode` and drives a `switch` in
/// `assets/voxygen/shaders/particle-vert.glsl`. A variant whose number has no
/// matching `case` in that shader falls through to the shader's `default:` arm
/// and renders as generic white drifting motes, with no warning anywhere — see
/// the `shader_modes_match_particle_modes` test below, which guards exactly
/// that.
#[derive(Copy, Clone)]
#[cfg_attr(test, derive(Debug, PartialEq, strum::EnumIter))]
pub enum ParticleMode {
    CampfireSmoke = 0,
    CampfireFire = 1,
    GunPowderSpark = 2,
    Shrapnel = 3,
    FireworkBlue = 4,
    FireworkGreen = 5,
    FireworkPurple = 6,
    FireworkRed = 7,
    FireworkWhite = 8,
    FireworkYellow = 9,
    Leaf = 10,
    Firefly = 11,
    Bee = 12,
    GroundShockwave = 13,
    EnergyHealing = 14,
    EnergyNature = 15,
    FlameThrower = 16,
    FireShockwave = 17,
    FireBowl = 18,
    Snow = 19,
    Explosion = 20,
    Ice = 21,
    LifestealBeam = 22,
    CultistFlame = 23,
    StaticSmoke = 24,
    Blood = 25,
    Enraged = 26,
    BigShrapnel = 27,
    Laser = 28,
    Bubbles = 29,
    Water = 30,
    IceSpikes = 31,
    Drip = 32,
    Tornado = 33,
    Death = 34,
    EnergyBuffing = 35,
    WebStrand = 36,
    BlackSmoke = 37,
    Lightning = 38,
    Steam = 39,
    BarrelOrgan = 40,
    PotionSickness = 41,
    GigaSnow = 42,
    CyclopsCharge = 43,
    SnowStorm = 44,
    PortalFizz = 45,
    Ink = 46,
    IceWhirlwind = 47,
    FieryBurst = 48,
    FieryBurstVortex = 49,
    FieryBurstSparks = 50,
    FieryBurstAsh = 51,
    FieryTornado = 52,
    PhoenixCloud = 53,
    FieryDropletTrace = 54,
    EnergyPhoenix = 55,
    PhoenixBeam = 56,
    PhoenixBuildUpAim = 57,
    ClayShrapnel = 58,
    Airflow = 59,
    Spore = 60,
    SurpriseEgg = 61,
    FlameTornado = 62,
    Poison = 63,
    WaterFoam = 64,
    EngineJet = 65,
    Transformation = 66,
    FireGigasAsh = 67,
    FireGigasWhirlwind = 68,
    FireGigasOverheat = 69,
    FireGigasExplosion = 70,
    FirePillarIndicator = 71,
    FirePillar = 72,
    FireLowShockwave = 73,
    PipeSmoke = 74,
    TrainSmoke = 75,
    Bubble = 76,
    ElephantVacuum = 77,
    ElectricSparks = 78,
    FlamethrowerBlue = 79,
    FlameCloakOrbit = 80,
    Dust = 81,
    CaveDust = 82,
    BubbleAmbient = 83,
    /// The Cromatolis Aerial Citadel's harmless practice beam
    /// (`common::comp::CitadelPracticeBeam`).
    CitadelLaser = 84,
    /// The Cromatolis Aerial Citadel's harmless practice sphere
    /// (`common::comp::CitadelPracticeSphere`).
    CitadelSphere = 85,
}

impl ParticleMode {
    pub fn into_uint(self) -> u32 { self as u32 }
}

#[repr(C)]
#[derive(Copy, Clone, Debug, Zeroable, Pod)]
pub struct Instance {
    // created_at time, so we can calculate time relativity, needed for relative animation.
    // can save 32 bits per instance, for particles that are not relatively animated.
    inst_time: f32,

    // The lifespan in seconds of the particle
    inst_lifespan: f32,

    // a seed value for randomness
    // can save 32 bits per instance, for particles that don't need randomness/uniqueness.
    inst_entropy: f32,

    // modes should probably be seperate shaders, as a part of scaling and optimisation efforts.
    // can save 32 bits per instance, and have cleaner tailor made code.
    inst_mode: i32,

    // A direction for particles to move in. Alternatively, a color.
    inst_dir_color: [f32; 3],

    // a triangle is: f32 x 3 x 3 x 1  = 288 bits
    // a quad is:     f32 x 3 x 3 x 2  = 576 bits
    // a cube is:     f32 x 3 x 3 x 12 = 3456 bits
    // this vec is:   f32 x 3 x 1 x 1  = 96 bits (per instance!)
    // consider using a throw-away mesh and
    // positioning the vertex verticies instead,
    // if we have:
    // - a triangle mesh, and 3 or more instances.
    // - a quad mesh, and 6 or more instances.
    // - a cube mesh, and 36 or more instances.
    inst_pos: [f32; 3],

    inst_start_wind_vel: [f32; 2],

    // The voxel lighting at the particle's expected position.
    //
    // First element is sunlight, second is glow light.
    //
    // If in doubt, use (1.0, 0.0).
    inst_voxel_light: [f32; 2],
}

impl Instance {
    pub fn new(
        inst_time: f64,
        lifespan: f32,
        inst_mode: ParticleMode,
        inst_pos: Vec3<f32>,
        inst_start_wind_vel: Vec2<f32>,
    ) -> Self {
        use rand::RngExt;
        Self {
            inst_time: (inst_time % super::TIME_OVERFLOW) as f32,
            inst_lifespan: lifespan,
            inst_entropy: rand::rng().random(),
            inst_mode: inst_mode as i32,
            inst_pos: inst_pos.into_array(),
            inst_start_wind_vel: inst_start_wind_vel.into_array(),
            inst_dir_color: [0.0, 0.0, 0.0],
            inst_voxel_light: [1.0, 0.0],
        }
    }

    pub fn new_directed(
        inst_time: f64,
        lifespan: f32,
        inst_mode: ParticleMode,
        inst_pos: Vec3<f32>,
        inst_pos2: Vec3<f32>,
        inst_start_wind_vel: Vec2<f32>,
    ) -> Self {
        use rand::RngExt;
        Self {
            inst_time: (inst_time % super::TIME_OVERFLOW) as f32,
            inst_lifespan: lifespan,
            inst_entropy: rand::rng().random(),
            inst_mode: inst_mode as i32,
            inst_pos: inst_pos.into_array(),
            inst_start_wind_vel: inst_start_wind_vel.into_array(),
            inst_dir_color: (inst_pos2 - inst_pos).into_array(),
            inst_voxel_light: [1.0, 0.0],
        }
    }

    pub fn new_colored(
        inst_time: f64,
        lifespan: f32,
        inst_mode: ParticleMode,
        inst_pos: Vec3<f32>,
        col: Rgb<f32>,
        inst_start_wind_vel: Vec2<f32>,
    ) -> Self {
        use rand::RngExt;
        Self {
            inst_time: (inst_time % super::TIME_OVERFLOW) as f32,
            inst_lifespan: lifespan,
            inst_entropy: rand::rng().random(),
            inst_mode: inst_mode as i32,
            inst_pos: inst_pos.into_array(),
            inst_start_wind_vel: inst_start_wind_vel.into_array(),
            inst_dir_color: col.into_array(),
            inst_voxel_light: [1.0, 0.0],
        }
    }

    pub fn with_light(self, sun_light: f32, glow_light: f32) -> Self {
        Self {
            inst_voxel_light: [sun_light, glow_light],
            ..self
        }
    }

    fn desc<'a>() -> wgpu::VertexBufferLayout<'a> {
        const ATTRIBUTES: [wgpu::VertexAttribute; 8] = wgpu::vertex_attr_array![2 => Float32, 3 => Float32, 4 => Float32, 5 => Sint32, 6 => Float32x3, 7 => Float32x3, 8 => Float32x2, 9 => Float32x2];
        wgpu::VertexBufferLayout {
            array_stride: mem::size_of::<Self>() as wgpu::BufferAddress,
            step_mode: wgpu::VertexStepMode::Instance,
            attributes: &ATTRIBUTES,
        }
    }
}

impl Default for Instance {
    fn default() -> Self {
        Self::new(
            0.0,
            0.0,
            ParticleMode::CampfireSmoke,
            Vec3::zero(),
            Vec2::zero(),
        )
    }
}

pub struct ParticlePipeline {
    pub pipeline: wgpu::RenderPipeline,
}

impl ParticlePipeline {
    pub fn new(
        device: &wgpu::Device,
        vs_module: &wgpu::ShaderModule,
        fs_module: &wgpu::ShaderModule,
        global_layout: &GlobalsLayouts,
        format: wgpu::TextureFormat,
        pipeline_modes: &PipelineModes,
    ) -> Self {
        common_base::span!(_guard, "ParticlePipeline::new");
        let render_pipeline_layout =
            device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: Some("Particle pipeline layout"),
                push_constant_ranges: &[],
                bind_group_layouts: &[&global_layout.globals, &global_layout.shadow_textures],
            });

        let samples = pipeline_modes.aa.samples();

        let render_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("Particle pipeline"),
            layout: Some(&render_pipeline_layout),
            vertex: wgpu::VertexState {
                module: vs_module,
                entry_point: Some("main"),
                buffers: &[Vertex::desc(), Instance::desc()],
                compilation_options: Default::default(),
            },
            primitive: wgpu::PrimitiveState {
                topology: wgpu::PrimitiveTopology::TriangleList,
                strip_index_format: None,
                front_face: wgpu::FrontFace::Ccw,
                cull_mode: Some(wgpu::Face::Back),
                unclipped_depth: false,
                polygon_mode: if pipeline_modes
                    .experimental_shaders
                    .contains(&ExperimentalShader::Wireframe)
                {
                    wgpu::PolygonMode::Line
                } else {
                    wgpu::PolygonMode::Fill
                },
                conservative: false,
            },
            depth_stencil: Some(wgpu::DepthStencilState {
                format: wgpu::TextureFormat::Depth32Float,
                depth_write_enabled: true,
                depth_compare: wgpu::CompareFunction::GreaterEqual,
                stencil: wgpu::StencilState {
                    front: wgpu::StencilFaceState::IGNORE,
                    back: wgpu::StencilFaceState::IGNORE,
                    read_mask: !0,
                    write_mask: 0,
                },
                bias: wgpu::DepthBiasState {
                    constant: 0,
                    slope_scale: 0.0,
                    clamp: 0.0,
                },
            }),
            multisample: wgpu::MultisampleState {
                count: samples,
                mask: !0,
                alpha_to_coverage_enabled: false,
            },
            fragment: Some(wgpu::FragmentState {
                module: fs_module,
                entry_point: Some("main"),
                targets: &[
                    Some(wgpu::ColorTargetState {
                        format,
                        blend: Some(wgpu::BlendState {
                            color: wgpu::BlendComponent {
                                src_factor: wgpu::BlendFactor::SrcAlpha,
                                dst_factor: wgpu::BlendFactor::OneMinusSrcAlpha,
                                operation: wgpu::BlendOperation::Add,
                            },
                            alpha: wgpu::BlendComponent {
                                src_factor: wgpu::BlendFactor::One,
                                dst_factor: wgpu::BlendFactor::One,
                                operation: wgpu::BlendOperation::Add,
                            },
                        }),
                        write_mask: wgpu::ColorWrites::ALL,
                    }),
                    Some(wgpu::ColorTargetState {
                        format: wgpu::TextureFormat::Rgba8Uint,
                        blend: None,
                        write_mask: wgpu::ColorWrites::ALL,
                    }),
                ],
                compilation_options: Default::default(),
            }),
            multiview: None,
            cache: None,
        });

        Self {
            pipeline: render_pipeline,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::ParticleMode;
    use std::collections::{HashMap, HashSet};
    use strum::IntoEnumIterator;

    /// `ParticleMode` variants that deliberately have no counterpart in
    /// `particle-vert.glsl`.
    ///
    /// `SnowStorm` is inherited from upstream Veloren, is constructed by
    /// nothing in the tree, and has never had a shader constant. Deleting
    /// it would widen the upstream-merge surface for no gain, so it is
    /// exempted here instead. **Nothing else belongs in this list**: a mode
    /// that is actually emitted but has no shader case is a bug (it renders
    /// as the shader's `default:` white motes), not an exemption.
    const MODES_WITHOUT_SHADER_CASE: &[ParticleMode] = &[ParticleMode::SnowStorm];

    const SHADER: &str = "voxygen/shaders/particle-vert.glsl";
    const SHADER_DIR: &str = "voxygen/shaders";

    fn shader_source(relative: &str) -> String {
        let path = common::assets::ASSETS_PATH.join(SHADER_DIR).join(relative);
        std::fs::read_to_string(&path)
            .unwrap_or_else(|err| panic!("failed to read {}: {err}", path.display()))
    }

    fn particle_vert_source() -> String { shader_source("particle-vert.glsl") }

    /// Fallible twin of `shader_source`, for use inside `shaderc`'s include
    /// callback: that callback is invoked from C, so it must return an error
    /// rather than unwind a panic across the FFI boundary.
    fn try_shader_source(relative: &str) -> Result<String, String> {
        let path = common::assets::ASSETS_PATH.join(SHADER_DIR).join(relative);
        std::fs::read_to_string(&path).map_err(|err| format!("{}: {err}", path.display()))
    }

    /// Every `const int NAME = N;` declaration in `src`, keyed by name.
    ///
    /// Deliberately whole-file rather than bounded to the mode block: the
    /// fragment shader keeps its own function-local copy of one mode number
    /// (`const int WATER_FOAM = 64;`), and that copy has to be checked too.
    /// Non-mode constants are filtered out by the caller, which only trusts a
    /// name that is also used as a `case` label.
    fn int_constants(src: &str) -> HashMap<String, u32> {
        let mut constants: HashMap<String, u32> = HashMap::new();
        for line in src.lines() {
            let Some(decl) = line
                .trim()
                .strip_prefix("const int ")
                .and_then(|decl| decl.strip_suffix(';'))
            else {
                continue;
            };
            let Some((name, value)) = decl.split_once(" = ") else {
                continue;
            };
            let Ok(value) = value.trim().parse::<u32>() else {
                continue;
            };
            constants.insert(name.trim().to_string(), value);
        }
        constants
    }

    /// The body of `particle-vert.glsl`'s `switch(inst_mode)`, so that `case`
    /// labels belonging to some other switch cannot satisfy the contract.
    fn mode_switch_body(src: &str) -> &str {
        let start = src
            .find("switch(inst_mode) {")
            .expect("particle-vert.glsl must contain `switch(inst_mode) {`");
        let mut depth = 0usize;
        for (offset, ch) in src[start..].char_indices() {
            match ch {
                '{' => depth += 1,
                '}' => {
                    depth -= 1;
                    if depth == 0 {
                        return &src[start..start + offset];
                    }
                },
                _ => {},
            }
        }
        panic!("particle-vert.glsl's `switch(inst_mode)` is unbalanced");
    }

    /// Each `case NAME:` label in the mode switch, paired with whether its arm
    /// reaches a `break;` before the next label.
    ///
    /// A `case` copied without its `break` falls through into the next mode's
    /// body — the effect silently renders as its neighbour.
    fn mode_switch_arms(switch_body: &str) -> HashMap<String, bool> {
        let mut arms: HashMap<String, bool> = HashMap::new();
        let mut pending: Option<String> = None;
        let mut broke = false;
        for line in switch_body.lines() {
            let line = line.trim();
            if let Some(label) = line.strip_prefix("case ").and_then(|l| l.strip_suffix(':')) {
                if let Some(previous) = pending.replace(label.trim().to_string()) {
                    arms.insert(previous, broke);
                }
                broke = false;
            } else if line == "default:" {
                if let Some(previous) = pending.take() {
                    arms.insert(previous, broke);
                }
                broke = false;
            } else if line == "break;" {
                broke = true;
            }
        }
        if let Some(previous) = pending {
            arms.insert(previous, broke);
        }
        arms
    }

    /// Every `ParticleMode` must be declared *and* handled by the particle
    /// vertex shader; the shader must not declare modes Rust cannot produce;
    /// each arm must `break`; and the fragment shader's own copy of a mode
    /// number must agree.
    ///
    /// Every one of those fails silently in the real renderer: a mode with no
    /// `case` renders as the `default:` arm's generic white motes, a missing
    /// `break` renders as the neighbouring effect, a stale shader constant is
    /// dead GLSL nobody notices, and a drifted fragment-shader constant
    /// mis-tints an unrelated effect. None produces a log line, a panic, or a
    /// compile error, which is why this is a test.
    ///
    /// What it cannot check: that a `case` body attached to the right number
    /// implements the intended *look*, or that an emission site passes the mode
    /// it meant to. Those stay human judgement.
    #[test]
    fn shader_modes_match_particle_modes() {
        let src = particle_vert_source();
        let switch_body = mode_switch_body(&src);
        let arms = mode_switch_arms(switch_body);
        let declared = int_constants(&src);

        // A constant only counts as a mode declaration if the switch also
        // dispatches on it, so an unrelated `const int` cannot trip this test.
        let mut by_value: HashMap<u32, &String> = HashMap::new();
        for (name, value) in &declared {
            if !arms.contains_key(name) {
                continue;
            }
            if let Some(previous) = by_value.insert(*value, name) {
                panic!("{SHADER}: mode {value} is declared by both {previous} and {name}");
            }
        }

        for mode in ParticleMode::iter() {
            let value = mode.into_uint();
            if MODES_WITHOUT_SHADER_CASE.contains(&mode) {
                assert!(
                    !by_value.contains_key(&value),
                    "{mode:?} ({value}) now has a shader case; remove it from \
                     MODES_WITHOUT_SHADER_CASE",
                );
                continue;
            }

            let name = by_value.get(&value).copied().unwrap_or_else(|| {
                panic!(
                    "{mode:?} ({value}) has no `const int … = {value};` with a matching `case` in \
                     {SHADER}, so it renders as the shader's `default:` white motes",
                )
            });
            assert!(
                arms[name],
                "{SHADER}: `case {name}:` ({mode:?}) has no `break;`, so it falls through into \
                 the next mode's body",
            );
        }

        let known: HashSet<u32> = ParticleMode::iter().map(ParticleMode::into_uint).collect();
        for (value, name) in &by_value {
            assert!(
                known.contains(value),
                "{SHADER} dispatches on `{name} = {value}` but no `ParticleMode` uploads that \
                 number",
            );
        }

        // `particle-frag.glsl` redeclares one mode number locally to pick a
        // deferred material; it must not drift from the vertex shader's value.
        for (name, value) in int_constants(&shader_source("particle-frag.glsl")) {
            if let Some(expected) = declared.get(&name) {
                assert_eq!(
                    value, *expected,
                    "particle-frag.glsl declares `{name} = {value}` but {SHADER} declares `{name} \
                     = {expected}`",
                );
            } else {
                panic!(
                    "particle-frag.glsl declares `{name} = {value}`, which {SHADER} does not \
                     declare at all — one of the two has drifted",
                );
            }
        }
    }

    /// Resolves an `#include <…>` exactly as the renderer's `fetch_include`
    /// does (`renderer::pipeline_creation`): a fixed whitelist, with `cloud`
    /// and `anti-aliasing` standing in for a graphics setting, and an error
    /// for anything else — so adding an include the renderer cannot resolve
    /// fails here rather than at client launch.
    fn resolve_include(name: &str, constants: &str) -> Result<String, String> {
        match name {
            "constants.glsl" => Ok(constants.to_string()),
            "cloud.glsl" => try_shader_source("include/cloud/regular.glsl"),
            "anti-aliasing.glsl" => try_shader_source("antialias/none.glsl"),
            "globals.glsl"
            | "shadows.glsl"
            | "rain_occlusion.glsl"
            | "sky.glsl"
            | "light.glsl"
            | "srgb.glsl"
            | "random.glsl"
            | "lod.glsl"
            | "point_glow.glsl"
            | "fxaa.glsl" => try_shader_source(&format!("include/{name}")),
            other => Err(format!(
                "include <{other}> is not in the renderer's whitelist, so the client would refuse \
                 to compile this shader"
            )),
        }
    }

    /// The subset of `ShaderModules::new`'s generated prelude that the particle
    /// shaders' include chain reads. One fixed configuration on purpose — this
    /// is a syntax gate, not a matrix.
    fn constants_prelude() -> String {
        format!(
            "{}\n#define VOXYGEN_COMPUTATION_PREFERENCE \
             VOXYGEN_COMPUTATION_PREFERENCE_FRAGMENT\n#define FLUID_MODE \
             FLUID_MODE_MEDIUM\n#define CLOUD_MODE CLOUD_MODE_MEDIUM\n#define REFLECTION_MODE \
             REFLECTION_MODE_MEDIUM\n#define LIGHTING_ALGORITHM \
             LIGHTING_ALGORITHM_ASHIKHMIN\n#define SHADOW_MODE SHADOW_MODE_MAP\n#define \
             SSAO_QUALITY SSAO_QUALITY_MEDIUM\n",
            shader_source("include/constants.glsl"),
        )
    }

    /// The particle shaders are only ever compiled at runtime, inside a live
    /// client with a GPU (`renderer::pipeline_creation`). A GLSL error
    /// therefore costs a full client launch to discover, and on a hot
    /// reload it is swallowed into a single `error!` line while the old
    /// pipeline keeps running.
    ///
    /// This runs both particle shaders through `shaderc`, the renderer's
    /// *fallback* compiler — the default is naga unless
    /// `VELOREN_DISABLE_NAGA_SHADERS` is set (`render::mod`,
    /// `PipelineModes::enable_naga`), which `particle_shaders_parse_with_naga`
    /// below covers. `shaderc` is the stricter of the two and gives the better
    /// error message, with an exact line number.
    ///
    /// Blind spots, so nobody over-trusts this gate: it compiles one define
    /// configuration, so the `#ifdef EXPERIMENTAL_CURVEDWORLD` arm of
    /// `particle-vert.glsl` and the `#ifdef EXPERIMENTAL_BAREMINIMUM` arm of
    /// `particle-frag.glsl` are never compiled here; and nothing about this
    /// says how an effect looks.
    #[test]
    fn particle_shaders_compile() {
        let constants = constants_prelude();

        let compiler = shaderc::Compiler::new().expect("shaderc unavailable");
        let mut options = shaderc::CompileOptions::new().expect("shaderc options");
        options.set_optimization_level(shaderc::OptimizationLevel::Zero);
        options.set_forced_version_profile(430, shaderc::GlslProfile::Core);
        options.set_include_callback(move |name, _, from, _| {
            Ok(shaderc::ResolvedInclude {
                resolved_name: name.to_string(),
                content: resolve_include(name, &constants)
                    .map_err(|err| format!("include <{name}> in {from}: {err}"))?,
            })
        });

        for (file, kind) in [
            ("particle-vert.glsl", shaderc::ShaderKind::Vertex),
            ("particle-frag.glsl", shaderc::ShaderKind::Fragment),
        ] {
            compiler
                .compile_into_spirv(&shader_source(file), kind, file, "main", Some(&options))
                .unwrap_or_else(|err| panic!("{file} failed to compile:\n{err}"));
        }
    }

    /// The renderer's *default* shader path is naga, not `shaderc`
    /// (`render::mod`'s `enable_naga`, honoured in
    /// `renderer::pipeline_creation`). naga's GLSL frontend accepts a different
    /// dialect, so a shader that `shaderc` compiles can still fail for an
    /// ordinary player. This parses both particle shaders the way
    /// `WgpuCompiler` does — the same recursive regex include expansion, no
    /// extra defines — through naga's frontend directly, which needs no GPU.
    #[test]
    fn particle_shaders_parse_with_naga() {
        let constants = constants_prelude();
        let include = regex::Regex::new("(?mR)^#include +<(.+)>$").expect("include regex");

        for (file, stage) in [
            ("particle-vert.glsl", wgpu::naga::ShaderStage::Vertex),
            ("particle-frag.glsl", wgpu::naga::ShaderStage::Fragment),
        ] {
            let mut source = shader_source(file);
            // `WgpuCompiler` expands includes repeatedly until none remain.
            loop {
                let mut failure = None;
                let expanded = include
                    .replace_all(&source, |captured: &regex::Captures| match resolve_include(
                        &captured[1],
                        &constants,
                    ) {
                        Ok(content) => content,
                        Err(err) => {
                            failure = Some(err);
                            String::new()
                        },
                    })
                    .into_owned();
                if let Some(err) = failure {
                    panic!("{file}: {err}");
                }
                if expanded == source {
                    break;
                }
                source = expanded;
            }

            wgpu::naga::front::glsl::Frontend::default()
                .parse(&wgpu::naga::front::glsl::Options::from(stage), &source)
                .unwrap_or_else(|err| {
                    panic!("{file} failed to parse with naga (the default compiler):\n{err:?}")
                });
        }
    }
}
