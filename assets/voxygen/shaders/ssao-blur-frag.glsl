#version 440 core

layout(set = 0, binding = 0)
uniform texture2D t_src_ao;
layout(set = 0, binding = 1)
uniform sampler s_src_ao;

layout(set = 0, binding = 2)
uniform texture2D t_src_depth;
layout(set = 0, binding = 3)
uniform sampler s_src_depth;

layout(location = 0) in vec2 uv;

layout(location = 0) out vec4 tgt_ao;

// How many raw (non-linear, reverse-Z) depth-buffer units of difference
// before a neighbouring tap's contribution is fully discarded. The blur
// pass's bind group intentionally carries no view/projection matrices (it
// only reads the AO target + depth, no globals), so this compares the raw
// buffer value rather than reconstructing view-space distance -- adjacent
// half-res texels are close enough in screen space that the non-linearity
// doesn't meaningfully distort which side of an edge they're on, and this
// avoids adding a locals uniform + globals bind group just for the blur.
const float SSAO_BLUR_DEPTH_SIGMA = 0.02;

float raw_depth_at(vec2 uv) {
    uvec2 sz = textureSize(sampler2D(t_src_depth, s_src_depth), 0);
    return texelFetch(sampler2D(t_src_depth, s_src_depth), clamp(ivec2(uv * sz), ivec2(0), ivec2(sz) - 1), 0).x;
}

// Four taps -- two horizontal, two vertical, evaluated together in a single
// pass instead of as two separate encoder passes (a true H-then-V separable
// blur, the plan's other named option, would need a second half-res render
// target and draw call; that plumbing wasn't part of this shader change, and
// its cost-vs-quality tradeoff against this shape can't be measured without
// a GPU). Each tap is weighted by how close its depth is to the centre
// pixel's, so the blur fades out instead of crossing a depth discontinuity
// (an object silhouette, a terrain edge) -- the cross-bilateral requirement.
void main() {
    uvec2 ao_sz = textureSize(sampler2D(t_src_ao, s_src_ao), 0);
    vec2 texel = 1.0 / vec2(ao_sz);

    float center_ao = texture(sampler2D(t_src_ao, s_src_ao), uv).r;
    float center_depth = raw_depth_at(uv);

    vec2 offsets[4] = vec2[](
        vec2(texel.x, 0.0),
        vec2(-texel.x, 0.0),
        vec2(0.0, texel.y),
        vec2(0.0, -texel.y)
    );

    float sum = center_ao;
    float weight_sum = 1.0;
    for (int i = 0; i < 4; i++) {
        vec2 tap_uv = uv + offsets[i];
        float tap_ao = texture(sampler2D(t_src_ao, s_src_ao), tap_uv).r;
        float tap_depth = raw_depth_at(tap_uv);

        float depth_diff = tap_depth - center_depth;
        float weight = exp(-(depth_diff * depth_diff) / (2.0 * SSAO_BLUR_DEPTH_SIGMA * SSAO_BLUR_DEPTH_SIGMA));

        sum += tap_ao * weight;
        weight_sum += weight;
    }

    tgt_ao = vec4(sum / weight_sum);
}
