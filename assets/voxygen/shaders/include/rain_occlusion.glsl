
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

// Colour of an individual falling rain streak / snowflake, for the
// screen-space precipitation march in `clouds-frag.glsl`.
//
// NOT the same thing as the distance-haze tints in `include/cloud/flat.glsl`,
// which multiply sky light rather than colouring a drop, and which that file
// names separately — it includes `sky.glsl`, not this header. Retuning "the
// colour of snow" means touching both.
const vec3 RAIN_TINT = vec3(0.3, 0.35, 0.5);
const vec3 SNOW_TINT = vec3(0.92, 0.95, 1.0);

// The rest of how one drop looks, kept beside its colour so retuning the
// snowfall means editing one block rather than hunting through the march.
// Each is `mix`ed from the rain value to the snow value by the snow fraction.
//
// Aspect stretches the drop in the march's wall space: rain is a tall thin
// streak, a snowflake is roughly round. Radius is that drop's squared
// half-size, so snowflakes read a little larger on screen than raindrops, and
// alpha is how strongly a drop tints what is behind it.
const vec2 RAIN_DROP_ASPECT = vec2(4.0, 0.3);
const vec2 SNOW_DROP_ASPECT = vec2(2.2, 2.2);
const float RAIN_DROP_RADIUS_SQR = 0.01;
const float SNOW_DROP_RADIUS_SQR = 0.035;
const float RAIN_DROP_ALPHA = 0.5;
const float SNOW_DROP_ALPHA = 0.75;

float rain_occlusion_at(in vec3 fragPos)
{
    vec4 rain_pos = rain_occlusion_texture_mat * vec4(fragPos, 1.0);

    float visibility = textureProj(sampler2DShadow(t_directed_occlusion_maps, s_directed_occlusion_maps), rain_pos);

    return visibility;
}
#endif
