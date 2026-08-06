#version 440 core

layout(location = 0) in vec2 uv;

layout(location = 0) out vec4 tgt_ao;

// Always fully lit (no occlusion). The AO math has not been implemented yet;
// this deliberately reads nothing from its bound depth/material/locals
// inputs so the pass is provably a no-op regardless of what those buffers
// contain.
void main() {
    tgt_ao = vec4(1.0);
}
