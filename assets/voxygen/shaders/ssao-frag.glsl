#version 440 core

#include <constants.glsl>
#include <globals.glsl>
#include <random.glsl>

layout(set = 1, binding = 0)
uniform texture2D t_src_depth;
layout(set = 1, binding = 1)
uniform sampler s_src_depth;

layout (std140, set = 1, binding = 2)
uniform u_locals {
    mat4 all_mat_inv;
};

layout(set = 1, binding = 3)
uniform utexture2D t_src_mat;

layout(location = 0) in vec2 uv;

layout(location = 0) out vec4 tgt_ao;

// Copied verbatim from `clouds-frag.glsl` (its own `wpos_at()` / `depth_at()`):
// reverse-Z means near = 1.0, far = 0.0, and `buf_depth == 0.0` is the sky
// sentinel. Deriving these independently instead of copying them is exactly
// how this direction gets flipped by mistake.
vec3 wpos_at(vec2 uv) {
    uvec2 sz = textureSize(sampler2D(t_src_depth, s_src_depth), 0);
    float buf_depth = texelFetch(sampler2D(t_src_depth, s_src_depth), clamp(ivec2(uv * sz), ivec2(0), ivec2(sz) - 1), 0).x;
    vec4 clip_space = vec4((uv * 2.0 - 1.0) * vec2(1, -1), buf_depth, 1.0);
    vec4 view_space = all_mat_inv * clip_space;
    view_space /= view_space.w;
    return view_space.xyz;
}

float depth_at(vec2 uv) {
    uvec2 sz = textureSize(sampler2D(t_src_depth, s_src_depth), 0);
    float buf_depth = texelFetch(sampler2D(t_src_depth, s_src_depth), clamp(ivec2(uv * sz), ivec2(0), ivec2(sz) - 1), 0).x;
    if (buf_depth == 0.0) {
        return 524288.0;
    } else {
        vec4 clip_space = vec4((uv * 2.0 - 1.0) * vec2(1, -1), buf_depth, 1.0);
        vec4 view_space = all_mat_inv * clip_space;
        view_space /= view_space.w;
        return -(view_mat * view_space).z;
    }
}

// Named constants -- never inline literals in the sampling math below.

// World-space hemisphere radius (voxel-scale: a block is 1 world unit), so
// the effect reaches about one block from the surface -- enough for
// character-to-terrain and object-to-object contact shadows without
// darkening whole rooms.
const float SSAO_RADIUS = 1.0;

// `mix(1.0, ao, SSAO_STRENGTH)` never reaches 0: the forward shaders already
// combine emitted + reflected light into `tgt_color` (no separate ambient
// channel exists), so SSAO here approximates by multiplying *total* outgoing
// radiance rather than just the ambient term. A strength floor keeps that
// approximation from ever fully blacking out direct light.
const float SSAO_STRENGTH = 0.85;

// Pushes each hemisphere sample's origin along the surface normal before
// projecting it, so a flat surface doesn't self-occlude from depth-buffer
// quantization noise.
const float SSAO_NORMAL_BIAS = 0.05;

// Soft transition width (view-space world units) for the per-tap
// occluded/unoccluded test, so it ramps instead of hard-cutting at an
// arbitrary depth threshold.
const float SSAO_DEPTH_BIAS = 0.05;

// Vogel-spiral tap rotation: samples advance by the golden angle so no two
// tap counts ever realign into visible rings.
const float SSAO_GOLDEN_ANGLE = 2.39996323;
const float SSAO_TAU = 6.28318530718;

#if (SSAO_QUALITY == SSAO_QUALITY_LOW)
    const int SSAO_TAPS = 8;
#elif (SSAO_QUALITY == SSAO_QUALITY_HIGH)
    const int SSAO_TAPS = 32;
#else
    const int SSAO_TAPS = 16;
#endif

void main() {
    uvec2 mat_sz = textureSize(usampler2D(t_src_mat, s_src_depth), 0);
    uvec4 mat = texelFetch(usampler2D(t_src_mat, s_src_depth), clamp(ivec2(uv * mat_sz), ivec2(0), ivec2(mat_sz) - 1), 0);

    // Sky is never occluded, and this check runs before anything else reads
    // depth or normals: there is nothing behind the sky to reconstruct.
    if (mat.a == MAT_SKY) {
        tgt_ao = vec4(1.0);
        return;
    }

    vec3 wpos = wpos_at(uv);
    // The encoder writes `uvec3((f_norm + 1.0) * 127.0)`
    // (`terrain-frag.glsl`, `figure-frag.glsl`, `sprite-frag.glsl`); this is
    // its exact inverse. A second, disagreeing decode (`/255.0 - 0.5`) exists
    // in `clouds-frag.glsl`'s `norm_at()`, gated behind
    // `EXPERIMENTAL_OUTLINES` only -- do not copy it.
    vec3 norm = normalize(vec3(mat.xyz) / 127.0 - 1.0);

    vec3 up = abs(norm.z) < 0.999 ? vec3(0.0, 0.0, 1.0) : vec3(1.0, 0.0, 0.0);
    vec3 tangent = normalize(cross(up, norm));
    vec3 bitangent = cross(norm, tangent);
    mat3 tbn = mat3(tangent, bitangent, norm);

    // Per-pixel spiral rotation using the same procedural hash
    // `postprocess-frag.glsl` already uses for dithering, instead of a
    // rotation-noise texture -- this repo routes new binary assets through
    // Git LFS + the VPS store, which a small noise PNG doesn't need.
    float rotation = hash_two(uvec2(uv * vec2(mat_sz))) * SSAO_TAU;

    float view_z_origin = depth_at(uv);
    float occlusion_sum = 0.0;

    for (int i = 0; i < SSAO_TAPS; i++) {
        float fi = float(i) + 0.5;
        // sqrt-distributed radius covers the disk in equal-area annuli
        // instead of bunching taps near the centre.
        float tap_radius = sqrt(fi / float(SSAO_TAPS));
        float angle = fi * SSAO_GOLDEN_ANGLE + rotation;
        vec3 local_dir = vec3(
            tap_radius * cos(angle),
            tap_radius * sin(angle),
            sqrt(max(0.0, 1.0 - tap_radius * tap_radius))
        );
        vec3 sample_dir = tbn * local_dir;
        vec3 sample_pos = wpos + norm * SSAO_NORMAL_BIAS + sample_dir * SSAO_RADIUS;

        // A sample that projects behind the camera or off-screen has no
        // known occluder; skip it rather than guess. `SSAO_TAPS` in the
        // denominator below stays fixed, so a skipped tap counts as
        // unoccluded -- a conservative under-estimate at screen edges,
        // never a fabricated one.
        vec4 clip = all_mat * vec4(sample_pos, 1.0);
        if (clip.w <= 0.0) {
            continue;
        }
        vec2 suv = (clip.xy / clip.w) * 0.5 * vec2(1, -1) + 0.5;
        if (suv.x < 0.0 || suv.x > 1.0 || suv.y < 0.0 || suv.y > 1.0) {
            continue;
        }

        float scene_view_z = depth_at(suv);
        float sample_view_z = -(view_mat * vec4(sample_pos, 1.0)).z;

        // A tap is occluded when real geometry sits closer to the camera
        // than the virtual hemisphere sample -- something is blocking its
        // line of sight. Smooth, not a hard cutoff.
        float depth_diff = sample_view_z - scene_view_z;
        float occluded = smoothstep(0.0, SSAO_DEPTH_BIAS, depth_diff);

        // Smooth range check: fades an occluder's contribution to zero once
        // it sits more than SSAO_RADIUS away (in view-space depth) from the
        // origin fragment, so unrelated distant geometry behind the sample
        // can't create a halo.
        float range_check = smoothstep(0.0, 1.0, SSAO_RADIUS / max(0.001, abs(view_z_origin - scene_view_z)));

        occlusion_sum += occluded * range_check;
    }

    float occlusion = 1.0 - SSAO_STRENGTH * (occlusion_sum / float(SSAO_TAPS));
    tgt_ao = vec4(occlusion);
}
