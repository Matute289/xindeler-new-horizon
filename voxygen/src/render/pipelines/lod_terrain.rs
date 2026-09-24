use super::super::{
    ExperimentalShader, GlobalsLayouts, PipelineModes, Renderer, Texture, Vertex as VertexTrait,
};
use bytemuck::{Pod, Zeroable};
use std::mem;
use vek::*;

#[repr(C)]
#[derive(Copy, Clone, Debug, Zeroable, Pod)]
pub struct Vertex {
    pos: [f32; 2],
}

impl Vertex {
    pub fn new(pos: Vec2<f32>) -> Self {
        Self {
            pos: pos.into_array(),
        }
    }

    fn desc<'a>() -> wgpu::VertexBufferLayout<'a> {
        const ATTRIBUTES: [wgpu::VertexAttribute; 1] = wgpu::vertex_attr_array![0 => Float32x2];
        wgpu::VertexBufferLayout {
            array_stride: Self::STRIDE,
            step_mode: wgpu::VertexStepMode::Vertex,
            attributes: &ATTRIBUTES,
        }
    }
}

impl VertexTrait for Vertex {
    const QUADS_INDEX: Option<wgpu::IndexFormat> = Some(wgpu::IndexFormat::Uint32);
    const STRIDE: wgpu::BufferAddress = mem::size_of::<Self>() as wgpu::BufferAddress;
}

pub struct LodData {
    pub map: Texture,
    pub alt: Texture,
    pub horizon: Texture,
    pub tgt_detail: u32,
    pub weather: Texture,
}

impl LodData {
    pub fn dummy(renderer: &mut Renderer) -> Self {
        let map_size = Vec2::new(1, 1);
        //let map_border = [0.0, 0.0, 0.0, 0.0];
        let map_image = [0];
        let alt_image = [0];
        let horizon_image = [0x_00_01_00_01];

        Self::new(
            renderer,
            map_size,
            &map_image,
            &alt_image,
            &horizon_image,
            Vec2::new(1, 1),
            1,
            //map_border.into(),
        )
    }

    pub fn new(
        renderer: &mut Renderer,
        map_size: Vec2<u32>,
        lod_base: &[u32],
        lod_alt: &[u32],
        lod_horizon: &[u32],
        weather_size: Vec2<u32>,
        tgt_detail: u32,
        //border_color: gfx::texture::PackedColor,
    ) -> Self {
        let mut create_texture = |format, data, filter| {
            let texture_info = wgpu::TextureDescriptor {
                label: None,
                size: wgpu::Extent3d {
                    width: map_size.x,
                    height: map_size.y,
                    depth_or_array_layers: 1,
                },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format,
                usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
                view_formats: &[],
            };

            let sampler_info = wgpu::SamplerDescriptor {
                label: None,
                address_mode_u: wgpu::AddressMode::ClampToEdge,
                address_mode_v: wgpu::AddressMode::ClampToEdge,
                address_mode_w: wgpu::AddressMode::ClampToEdge,
                mag_filter: filter,
                min_filter: filter,
                mipmap_filter: wgpu::FilterMode::Nearest,
                border_color: Some(wgpu::SamplerBorderColor::TransparentBlack),
                ..Default::default()
            };

            let view_info = wgpu::TextureViewDescriptor {
                label: None,
                format: Some(format),
                dimension: Some(wgpu::TextureViewDimension::D2),
                usage: None,
                aspect: wgpu::TextureAspect::All,
                base_mip_level: 0,
                mip_level_count: None,
                base_array_layer: 0,
                array_layer_count: None,
            };

            renderer.create_texture_with_data_raw(
                &texture_info,
                &view_info,
                &sampler_info,
                bytemuck::cast_slice(data),
            )
        };
        let map = create_texture(
            wgpu::TextureFormat::Rgba8UnormSrgb,
            lod_base,
            wgpu::FilterMode::Linear,
        );
        //             SamplerInfo {
        //                 border: border_color,
        let alt = create_texture(
            wgpu::TextureFormat::Rgba8Unorm,
            lod_alt,
            wgpu::FilterMode::Linear,
        );
        //             SamplerInfo {
        //                 border: [0.0, 0.0, 0.0, 0.0].into(),
        let horizon = create_texture(
            wgpu::TextureFormat::Rgba8Unorm,
            lod_horizon,
            wgpu::FilterMode::Linear,
        );
        //             SamplerInfo {
        //                 border: [1.0, 0.0, 1.0, 0.0].into(),
        let weather = {
            let texture_info = wgpu::TextureDescriptor {
                label: None,
                size: wgpu::Extent3d {
                    width: weather_size.x,
                    height: weather_size.y,
                    depth_or_array_layers: 1,
                },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: wgpu::TextureFormat::Rgba8Unorm,
                usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
                view_formats: &[],
            };

            let sampler_info = wgpu::SamplerDescriptor {
                label: None,
                address_mode_u: wgpu::AddressMode::ClampToBorder,
                address_mode_v: wgpu::AddressMode::ClampToBorder,
                address_mode_w: wgpu::AddressMode::ClampToBorder,
                mag_filter: wgpu::FilterMode::Linear,
                min_filter: wgpu::FilterMode::Linear,
                mipmap_filter: wgpu::FilterMode::Nearest,
                border_color: Some(wgpu::SamplerBorderColor::TransparentBlack),
                ..Default::default()
            };

            let view_info = wgpu::TextureViewDescriptor {
                label: None,
                format: Some(wgpu::TextureFormat::Rgba8Unorm),
                dimension: Some(wgpu::TextureViewDimension::D2),
                usage: None,
                aspect: wgpu::TextureAspect::All,
                base_mip_level: 0,
                mip_level_count: None,
                base_array_layer: 0,
                array_layer_count: None,
            };

            renderer.create_texture_with_data_raw(
                &texture_info,
                &view_info,
                &sampler_info,
                vec![0; weather_size.x as usize * weather_size.y as usize * 4].as_slice(),
            )
        };
        Self {
            map,
            alt,
            horizon,
            tgt_detail,
            weather,
        }
    }
}

pub struct LodTerrainPipeline {
    pub pipeline: wgpu::RenderPipeline,
}

impl LodTerrainPipeline {
    pub fn new(
        device: &wgpu::Device,
        vs_module: &wgpu::ShaderModule,
        fs_module: &wgpu::ShaderModule,
        global_layout: &GlobalsLayouts,
        format: wgpu::TextureFormat,
        pipeline_modes: &PipelineModes,
    ) -> Self {
        let render_pipeline_layout =
            device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: Some("Lod terrain pipeline layout"),
                push_constant_ranges: &[],
                bind_group_layouts: &[&global_layout.globals, &global_layout.shadow_textures],
            });

        let samples = pipeline_modes.aa.samples();

        let render_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("Lod terrain pipeline"),
            layout: Some(&render_pipeline_layout),
            vertex: wgpu::VertexState {
                module: vs_module,
                entry_point: Some("main"),
                buffers: &[Vertex::desc()],
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
                        blend: None,
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
    //! Offline shader-compile gate for the LoD terrain shaders, mirroring
    //! `pipelines::particle`'s `particle_shaders_compile` /
    //! `particle_shaders_parse_with_naga` pair. These shaders are only ever
    //! compiled at runtime inside a live client with a GPU
    //! (`renderer::pipeline_creation`), so a GLSL error otherwise costs a
    //! full client launch to discover — and on a hot reload it is swallowed
    //! into a single `error!` line while the old pipeline keeps running.
    //!
    //! Added alongside the `lod_pos()` relaxation-loop fix in
    //! `include/lod.glsl` (bounding the per-iteration "push toward local
    //! optima" step, which could otherwise blow up on a steep real cliff and
    //! produce a degenerate LoD triangle) so a future edit to that function
    //! fails a fast, GPU-less test instead of only showing up as an in-game
    //! artifact.

    const SHADER_DIR: &str = "voxygen/shaders";

    fn shader_source(relative: &str) -> String {
        let path = common::assets::ASSETS_PATH.join(SHADER_DIR).join(relative);
        std::fs::read_to_string(&path)
            .unwrap_or_else(|err| panic!("failed to read {}: {err}", path.display()))
    }

    /// Fallible twin of `shader_source`, for use inside `shaderc`'s include
    /// callback: that callback is invoked from C, so it must return an error
    /// rather than unwind a panic across the FFI boundary.
    fn try_shader_source(relative: &str) -> Result<String, String> {
        let path = common::assets::ASSETS_PATH.join(SHADER_DIR).join(relative);
        std::fs::read_to_string(&path).map_err(|err| format!("{}: {err}", path.display()))
    }

    /// Resolves an `#include <…>` exactly as the renderer's `fetch_include`
    /// does (`renderer::pipeline_creation`): a fixed whitelist, with `cloud`
    /// standing in for a graphics setting, and an error for anything else —
    /// so adding an include the renderer cannot resolve fails here rather
    /// than at client launch.
    fn resolve_include(name: &str, constants: &str) -> Result<String, String> {
        match name {
            "constants.glsl" => Ok(constants.to_string()),
            "cloud.glsl" => try_shader_source("include/cloud/regular.glsl"),
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

    /// The subset of `ShaderModules::new`'s generated prelude that the LoD
    /// terrain shaders' include chain reads. One fixed configuration on
    /// purpose — this is a syntax gate, not a matrix.
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

    /// Runs both LoD terrain shaders through `shaderc`, the renderer's
    /// *fallback* compiler — the default is naga unless
    /// `VELOREN_DISABLE_NAGA_SHADERS` is set (`render::mod`,
    /// `PipelineModes::enable_naga`), which
    /// `lod_terrain_shaders_parse_with_naga` below covers. `shaderc` is the
    /// stricter of the two and gives the better error message, with an
    /// exact line number.
    #[test]
    fn lod_terrain_shaders_compile() {
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
            ("lod-terrain-vert.glsl", shaderc::ShaderKind::Vertex),
            ("lod-terrain-frag.glsl", shaderc::ShaderKind::Fragment),
        ] {
            compiler
                .compile_into_spirv(&shader_source(file), kind, file, "main", Some(&options))
                .unwrap_or_else(|err| panic!("{file} failed to compile:\n{err}"));
        }
    }

    /// The renderer's *default* shader path is naga, not `shaderc`
    /// (`render::mod`'s `enable_naga`, honoured in
    /// `renderer::pipeline_creation`). naga's GLSL frontend accepts a
    /// different dialect, so a shader that `shaderc` compiles can still fail
    /// for an ordinary player. This parses both LoD terrain shaders the way
    /// `WgpuCompiler` does — the same recursive regex include expansion, no
    /// extra defines — through naga's frontend directly, which needs no GPU.
    #[test]
    fn lod_terrain_shaders_parse_with_naga() {
        let constants = constants_prelude();
        let include = regex::Regex::new("(?mR)^#include +<(.+)>$").expect("include regex");

        for (file, stage) in [
            ("lod-terrain-vert.glsl", wgpu::naga::ShaderStage::Vertex),
            ("lod-terrain-frag.glsl", wgpu::naga::ShaderStage::Fragment),
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
