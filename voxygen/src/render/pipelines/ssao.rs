//! Screen-space ambient occlusion pipeline plumbing. `SsaoLayout` binds
//! depth + the material G-buffer + locals, which `ssao-frag.glsl` uses to
//! reconstruct world-space position and surface normals and compute real
//! hemisphere-sampled occlusion. `SsaoBlurLayout` additionally binds depth
//! so the blur pass can weight its taps by depth similarity (cross-bilateral
//! blur) instead of blurring across depth discontinuities / object edges.

use super::{super::Consts, GlobalsLayouts};
use bytemuck::{Pod, Zeroable};
use vek::*;

/// Shared by both AO targets (`tgt_ao`, `tgt_ao_blur`) and the pipelines that
/// render into them, so the renderer's target creation and the pipeline's
/// color target format can't drift apart. Single-channel is enough for an
/// occlusion factor.
pub const SSAO_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::R8Unorm;

#[repr(C)]
#[derive(Copy, Clone, Debug, Zeroable, Pod)]
pub struct Locals {
    all_mat_inv: [[f32; 4]; 4],
}

impl Default for Locals {
    fn default() -> Self { Self::new(Mat4::identity(), Mat4::identity()) }
}

impl Locals {
    pub fn new(proj_mat_inv: Mat4<f32>, view_mat_inv: Mat4<f32>) -> Self {
        Self {
            all_mat_inv: (view_mat_inv * proj_mat_inv).into_col_arrays(),
        }
    }
}

pub struct BindGroup {
    pub(in super::super) bind_group: wgpu::BindGroup,
}

/// Depth + material G-buffer inputs for the AO-generation pass, copying the
/// binding shape `clouds::CloudsLayout` already uses for the same two
/// textures.
pub struct SsaoLayout {
    pub layout: wgpu::BindGroupLayout,
}

impl SsaoLayout {
    pub fn new(device: &wgpu::Device) -> Self {
        Self {
            layout: device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                label: None,
                entries: &[
                    // Depth source
                    wgpu::BindGroupLayoutEntry {
                        binding: 0,
                        visibility: wgpu::ShaderStages::FRAGMENT,
                        ty: wgpu::BindingType::Texture {
                            sample_type: wgpu::TextureSampleType::Float { filterable: false },
                            view_dimension: wgpu::TextureViewDimension::D2,
                            multisampled: false,
                        },
                        count: None,
                    },
                    wgpu::BindGroupLayoutEntry {
                        binding: 1,
                        visibility: wgpu::ShaderStages::FRAGMENT,
                        ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::NonFiltering),
                        count: None,
                    },
                    // Locals
                    wgpu::BindGroupLayoutEntry {
                        binding: 2,
                        visibility: wgpu::ShaderStages::FRAGMENT,
                        ty: wgpu::BindingType::Buffer {
                            ty: wgpu::BufferBindingType::Uniform,
                            has_dynamic_offset: false,
                            min_binding_size: None,
                        },
                        count: None,
                    },
                    // Materials source
                    wgpu::BindGroupLayoutEntry {
                        binding: 3,
                        visibility: wgpu::ShaderStages::FRAGMENT,
                        ty: wgpu::BindingType::Texture {
                            sample_type: wgpu::TextureSampleType::Uint,
                            view_dimension: wgpu::TextureViewDimension::D2,
                            multisampled: false,
                        },
                        count: None,
                    },
                ],
            }),
        }
    }

    pub fn bind(
        &self,
        device: &wgpu::Device,
        src_depth: &wgpu::TextureView,
        src_mat: &wgpu::TextureView,
        depth_sampler: &wgpu::Sampler,
        locals: &Consts<Locals>,
    ) -> BindGroup {
        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: None,
            layout: &self.layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(src_depth),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::Sampler(depth_sampler),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: locals.buf().as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 3,
                    resource: wgpu::BindingResource::TextureView(src_mat),
                },
            ],
        });

        BindGroup { bind_group }
    }
}

/// Input for the blur pass: the AO-generation pass's output texture, plus
/// depth so the blur can weight taps by depth similarity (cross-bilateral
/// blur, so it doesn't blur across depth discontinuities / object edges).
pub struct SsaoBlurLayout {
    pub layout: wgpu::BindGroupLayout,
}

impl SsaoBlurLayout {
    pub fn new(device: &wgpu::Device) -> Self {
        Self {
            layout: device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                label: None,
                entries: &[
                    wgpu::BindGroupLayoutEntry {
                        binding: 0,
                        visibility: wgpu::ShaderStages::FRAGMENT,
                        ty: wgpu::BindingType::Texture {
                            sample_type: wgpu::TextureSampleType::Float { filterable: true },
                            view_dimension: wgpu::TextureViewDimension::D2,
                            multisampled: false,
                        },
                        count: None,
                    },
                    wgpu::BindGroupLayoutEntry {
                        binding: 1,
                        visibility: wgpu::ShaderStages::FRAGMENT,
                        ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                        count: None,
                    },
                    // Depth source, for the cross-bilateral weight. Same
                    // sample-type shape as `SsaoLayout`'s own depth binding
                    // above.
                    wgpu::BindGroupLayoutEntry {
                        binding: 2,
                        visibility: wgpu::ShaderStages::FRAGMENT,
                        ty: wgpu::BindingType::Texture {
                            sample_type: wgpu::TextureSampleType::Float { filterable: false },
                            view_dimension: wgpu::TextureViewDimension::D2,
                            multisampled: false,
                        },
                        count: None,
                    },
                    wgpu::BindGroupLayoutEntry {
                        binding: 3,
                        visibility: wgpu::ShaderStages::FRAGMENT,
                        ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::NonFiltering),
                        count: None,
                    },
                ],
            }),
        }
    }

    pub fn bind(
        &self,
        device: &wgpu::Device,
        src_ao: &wgpu::TextureView,
        sampler: &wgpu::Sampler,
        src_depth: &wgpu::TextureView,
        depth_sampler: &wgpu::Sampler,
    ) -> BindGroup {
        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: None,
            layout: &self.layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(src_ao),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::Sampler(sampler),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: wgpu::BindingResource::TextureView(src_depth),
                },
                wgpu::BindGroupEntry {
                    binding: 3,
                    resource: wgpu::BindingResource::Sampler(depth_sampler),
                },
            ],
        });

        BindGroup { bind_group }
    }
}

pub struct SsaoPipelines {
    pub ssao: wgpu::RenderPipeline,
    pub blur: wgpu::RenderPipeline,
}

impl SsaoPipelines {
    pub fn new(
        device: &wgpu::Device,
        vs_module: &wgpu::ShaderModule,
        ssao_fs_module: &wgpu::ShaderModule,
        blur_fs_module: &wgpu::ShaderModule,
        global_layout: &GlobalsLayouts,
        ssao_layout: &SsaoLayout,
        blur_layout: &SsaoBlurLayout,
    ) -> Self {
        common_base::span!(_guard, "SsaoPipelines::new");

        let ssao_pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("SSAO pipeline layout"),
            push_constant_ranges: &[],
            bind_group_layouts: &[&global_layout.globals, &ssao_layout.layout],
        });
        // The blur pass only ever reads the AO-generation pass's output
        // texture (plus depth, for a depth-aware blur), never world-space
        // data, so it doesn't need the globals bind group.
        let blur_pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("SSAO blur pipeline layout"),
            push_constant_ranges: &[],
            bind_group_layouts: &[&blur_layout.layout],
        });

        let create_pipeline = |label, layout: &wgpu::PipelineLayout, fs_module| {
            device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some(label),
                layout: Some(layout),
                vertex: wgpu::VertexState {
                    module: vs_module,
                    entry_point: Some("main"),
                    buffers: &[],
                    compilation_options: Default::default(),
                },
                primitive: wgpu::PrimitiveState {
                    topology: wgpu::PrimitiveTopology::TriangleList,
                    strip_index_format: None,
                    front_face: wgpu::FrontFace::Ccw,
                    cull_mode: None,
                    unclipped_depth: false,
                    polygon_mode: wgpu::PolygonMode::Fill,
                    conservative: false,
                },
                depth_stencil: None,
                multisample: wgpu::MultisampleState {
                    count: 1,
                    mask: !0,
                    alpha_to_coverage_enabled: false,
                },
                fragment: Some(wgpu::FragmentState {
                    module: fs_module,
                    entry_point: Some("main"),
                    targets: &[Some(wgpu::ColorTargetState {
                        format: SSAO_FORMAT,
                        blend: None,
                        write_mask: wgpu::ColorWrites::ALL,
                    })],
                    compilation_options: Default::default(),
                }),
                multiview: None,
                cache: None,
            })
        };

        let ssao_pipeline = create_pipeline("SSAO pipeline", &ssao_pipeline_layout, ssao_fs_module);
        let blur_pipeline =
            create_pipeline("SSAO blur pipeline", &blur_pipeline_layout, blur_fs_module);

        Self {
            ssao: ssao_pipeline,
            blur: blur_pipeline,
        }
    }
}
