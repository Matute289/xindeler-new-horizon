#version 440 core

layout(set = 0, binding = 0)
uniform texture2D t_src_ao;
layout(set = 0, binding = 1)
uniform sampler s_src_ao;

layout(location = 0) in vec2 uv;

layout(location = 0) out vec4 tgt_ao;

// Passthrough: no blur weighting yet, just forwards the AO-generation pass's
// output unchanged. Since that pass currently writes a constant 1.0
// everywhere, this stays a no-op too.
void main() {
    tgt_ao = texture(sampler2D(t_src_ao, s_src_ao), uv);
}
