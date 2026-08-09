use crate::{
    render::RenderMode,
    window::{FullScreenSettings, WindowSettings},
};
use common::ViewDistances;
use serde::{Deserialize, Serialize};
use std::fmt;

#[derive(Copy, Clone, Serialize, Deserialize, Debug, PartialEq, Eq)]
pub enum Fps {
    Max(u32),
    Unlimited,
}

pub fn get_fps(max_fps: Fps) -> u32 {
    match max_fps {
        Fps::Max(x) => x,
        Fps::Unlimited => u32::MAX,
    }
}

impl fmt::Display for Fps {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match *self {
            Fps::Max(x) => write!(f, "{}", x),
            Fps::Unlimited => write!(f, "Unlimited"),
        }
    }
}

/// `GraphicsSettings` contains settings related to framerate and in-game
/// visuals.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct GraphicsSettings {
    pub terrain_view_distance: u32,
    pub entity_view_distance: u32,
    pub lod_distance: u32,
    pub sprite_render_distance: u32,
    pub particles_enabled: bool,
    pub weapon_trails_enabled: bool,
    pub figure_lod_render_distance: u32,
    pub max_fps: Fps,
    pub max_background_fps: Fps,
    pub fov: u16,
    pub gamma: f32,
    pub exposure: f32,
    pub ambiance: f32,
    pub render_mode: RenderMode,
    pub window: WindowSettings,
    pub fullscreen: FullScreenSettings,
    pub lod_detail: u32,
}

impl Default for GraphicsSettings {
    fn default() -> Self {
        Self {
            terrain_view_distance: 10,
            entity_view_distance: client::MAX_SELECTABLE_VIEW_DISTANCE,
            lod_distance: 200,
            sprite_render_distance: 100,
            particles_enabled: true,
            weapon_trails_enabled: true,
            figure_lod_render_distance: 300,
            max_fps: Fps::Max(60),
            max_background_fps: Fps::Max(30),
            fov: 70,
            gamma: 1.0,
            exposure: 1.0,
            ambiance: 0.5,
            render_mode: RenderMode::default(),
            window: WindowSettings::default(),
            fullscreen: FullScreenSettings::default(),
            lod_detail: 250,
        }
    }
}

impl GraphicsSettings {
    pub fn into_minimal(self) -> Self {
        use crate::render::*;
        Self {
            terrain_view_distance: 4,
            entity_view_distance: 4,
            lod_distance: 0,
            sprite_render_distance: 80,
            figure_lod_render_distance: 100,
            lod_detail: 80,
            render_mode: RenderMode {
                aa: AaMode::FxUpscale,
                cloud: CloudMode::Flat,
                reflection: ReflectionMode::Low,
                fluid: FluidMode::Low,
                lighting: LightingMode::Lambertian,
                shadow: ShadowMode::None,
                rain_occlusion: ShadowMapMode { resolution: 0.25 },
                bloom: BloomMode::Off,
                ssao: SsaoMode::Off,
                point_glow: 0.0,
                upscale_mode: UpscaleMode { factor: 0.35 },
                ..self.render_mode
            },
            ..self
        }
    }

    pub fn into_low(self) -> Self {
        use crate::render::*;
        Self {
            terrain_view_distance: 7,
            entity_view_distance: 7,
            lod_distance: 75,
            sprite_render_distance: 125,
            figure_lod_render_distance: 200,
            lod_detail: 180,
            render_mode: RenderMode {
                aa: AaMode::FxUpscale,
                cloud: CloudMode::Low,
                reflection: ReflectionMode::Medium,
                fluid: FluidMode::Low,
                lighting: LightingMode::Lambertian,
                shadow: ShadowMode::Cheap,
                rain_occlusion: ShadowMapMode { resolution: 0.25 },
                bloom: BloomMode::Off,
                ssao: SsaoMode::Off,
                point_glow: 0.2,
                upscale_mode: UpscaleMode { factor: 0.65 },
                ..self.render_mode
            },
            ..self
        }
    }

    pub fn into_medium(self) -> Self {
        use crate::render::*;
        Self {
            terrain_view_distance: 10,
            entity_view_distance: 10,
            lod_distance: 150,
            sprite_render_distance: 250,
            figure_lod_render_distance: 350,
            lod_detail: 250,
            render_mode: RenderMode {
                aa: AaMode::Fxaa,
                cloud: CloudMode::Medium,
                reflection: ReflectionMode::High,
                fluid: FluidMode::Medium,
                lighting: LightingMode::BlinnPhong,
                shadow: ShadowMode::Map(ShadowMapMode { resolution: 0.75 }),
                rain_occlusion: ShadowMapMode { resolution: 0.25 },
                bloom: BloomMode::On(BloomConfig {
                    factor: BloomFactor::Medium,
                    uniform_blur: false,
                }),
                ssao: SsaoMode::On(SsaoConfig {
                    quality: SsaoQuality::Low,
                    strength: 0.6,
                }),
                point_glow: 0.2,
                upscale_mode: UpscaleMode { factor: 0.85 },
                ..self.render_mode
            },
            ..self
        }
    }

    pub fn into_high(self) -> Self {
        use crate::render::*;
        Self {
            terrain_view_distance: 16,
            entity_view_distance: 16,
            lod_distance: 200,
            sprite_render_distance: 350,
            figure_lod_render_distance: 450,
            lod_detail: 325,
            render_mode: RenderMode {
                aa: AaMode::Fxaa,
                cloud: CloudMode::Medium,
                reflection: ReflectionMode::High,
                fluid: FluidMode::Medium,
                lighting: LightingMode::Ashikhmin,
                shadow: ShadowMode::Map(ShadowMapMode { resolution: 1.0 }),
                rain_occlusion: ShadowMapMode { resolution: 0.5 },
                bloom: BloomMode::On(BloomConfig {
                    factor: BloomFactor::Medium,
                    uniform_blur: true,
                }),
                ssao: SsaoMode::On(SsaoConfig {
                    quality: SsaoQuality::Medium,
                    strength: 0.6,
                }),
                point_glow: 0.2,
                upscale_mode: UpscaleMode { factor: 1.0 },
                ..self.render_mode
            },
            ..self
        }
    }

    pub fn into_ultra(self) -> Self {
        use crate::render::*;
        Self {
            terrain_view_distance: 16,
            entity_view_distance: 16,
            lod_distance: 450,
            sprite_render_distance: 800,
            figure_lod_render_distance: 600,
            lod_detail: 400,
            render_mode: RenderMode {
                aa: AaMode::Fxaa,
                cloud: CloudMode::High,
                reflection: ReflectionMode::High,
                fluid: FluidMode::High,
                lighting: LightingMode::Ashikhmin,
                shadow: ShadowMode::Map(ShadowMapMode { resolution: 1.75 }),
                rain_occlusion: ShadowMapMode { resolution: 0.5 },
                bloom: BloomMode::On(BloomConfig {
                    factor: BloomFactor::Medium,
                    uniform_blur: true,
                }),
                ssao: SsaoMode::On(SsaoConfig {
                    quality: SsaoQuality::High,
                    strength: 0.6,
                }),
                point_glow: 0.2,
                upscale_mode: UpscaleMode { factor: 1.25 },
                ..self.render_mode
            },
            ..self
        }
    }

    pub fn view_distances(&self) -> ViewDistances {
        ViewDistances {
            terrain: self.terrain_view_distance,
            entity: self.entity_view_distance,
        }
    }

    /// Choose a graphics preset based on the detected GPU.
    /// Called on first launch when no settings file exists.
    pub fn auto_detect(adapter_info: &wgpu::AdapterInfo) -> Self {
        let name = adapter_info.name.to_lowercase();
        let is_discrete = matches!(adapter_info.device_type, wgpu::DeviceType::DiscreteGpu);

        let preset = if !is_discrete {
            // Integrated / virtual / CPU fallback — minimal preset to stay playable
            Preset::Minimal
        } else {
            gpu_preset_from_name(&name)
        };

        tracing::info!(
            gpu = %adapter_info.name,
            ?preset,
            "Auto-detected graphics preset"
        );

        let base = Self::default();
        match preset {
            Preset::Minimal => base.into_minimal(),
            Preset::Low => base.into_low(),
            Preset::Medium => base.into_medium(),
            Preset::High => base.into_high(),
            Preset::Ultra => base.into_ultra(),
        }
    }
}

#[derive(Debug)]
enum Preset {
    Minimal,
    Low,
    Medium,
    High,
    Ultra,
}

fn gpu_preset_from_name(name: &str) -> Preset {
    // NVIDIA — match generation from model number
    if name.contains("nvidia") || name.contains("geforce") || name.contains("quadro") {
        // RTX 40xx
        if contains_any(name, &["rtx 4090", "rtx 4080", "rtx 4070 ti"]) {
            return Preset::Ultra;
        }
        if contains_any(name, &["rtx 40", "rtx 3090", "rtx 3080"]) {
            return Preset::High;
        }
        if contains_any(name, &["rtx 30", "rtx 2080", "rtx 2070", "rtx 2060"]) {
            return Preset::High;
        }
        if contains_any(name, &["rtx 20", "gtx 1080", "gtx 1070", "gtx 1660"]) {
            return Preset::Medium;
        }
        // GTX 10xx and older
        return Preset::Low;
    }

    // AMD — detect by RX series
    if name.contains("radeon") || name.contains("amd") {
        if contains_any(name, &[
            "rx 7900", "rx 7800", "rx 6950", "rx 6900", "rx 6800",
        ]) {
            return Preset::Ultra;
        }
        if contains_any(name, &["rx 7", "rx 6700", "rx 6750", "rx 6650", "rx 6600"]) {
            return Preset::High;
        }
        if contains_any(name, &["rx 5700", "rx 5600", "rx 5500", "rx 590", "rx 580"]) {
            return Preset::Medium;
        }
        return Preset::Low;
    }

    // Intel Arc
    if name.contains("arc") {
        if contains_any(name, &["a770", "a750"]) {
            return Preset::High;
        }
        if contains_any(name, &["a580", "a380"]) {
            return Preset::Medium;
        }
        return Preset::Low;
    }

    // Apple Silicon (Metal — discrete-ish but powerful)
    if name.contains("apple") {
        if contains_any(name, &[
            "m3 max", "m3 ultra", "m2 max", "m2 ultra", "m1 ultra",
        ]) {
            return Preset::High;
        }
        return Preset::Medium;
    }

    // Unknown discrete GPU — safe default
    Preset::Medium
}

fn contains_any(haystack: &str, needles: &[&str]) -> bool {
    needles.iter().any(|n| haystack.contains(n))
}
