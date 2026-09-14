
#ifndef RAIN_OCCLUSION_GLSL
#define RAIN_OCCLUSION_GLSL

// Use with sampler2DShadow
layout(set = 1, binding = 4)
uniform texture2D t_directed_occlusion_maps;
layout(set = 1, binding = 5)
uniform samplerShadow s_directed_occlusion_maps;

layout (std140, set = 0, binding = 14)
uniform u_rain_occlusion {
    mat4 rain_occlusion_matrices;
    mat4 rain_occlusion_texture_mat;
    mat4 rain_dir_mat;
    float integrated_rain_vel;
    // Precipitation at the camera, split by what form it is falling in. Their
    // sum is the total precipitation; which way the split falls is decided by
    // the temperature of the ground below, not by the weather cell.
    //
    // `rain-occlusion-directed-vert.glsl` and `rain-occlusion-figure-vert.glsl`
    // declare this same block themselves rather than including this file --
    // keep all three, and `render::pipelines::rain_occlusion::Locals`, in sync.
    float rain_density;
    float snow_density;
    float occlusion_dummy; // Fix alignment.
};

// Colour of falling rain streaks and of snowflakes, shared by everything that
// draws or hazes precipitation.
const vec3 RAIN_TINT = vec3(0.3, 0.35, 0.5);
const vec3 SNOW_TINT = vec3(0.92, 0.95, 1.0);

float rain_occlusion_at(in vec3 fragPos)
{
    vec4 rain_pos = rain_occlusion_texture_mat * vec4(fragPos, 1.0);

    float visibility = textureProj(sampler2DShadow(t_directed_occlusion_maps, s_directed_occlusion_maps), rain_pos);

    return visibility;
}
#endif
