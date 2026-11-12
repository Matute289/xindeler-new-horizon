---
name: xindeler-render-architecture
description: Use when designing or changing voxygen's Rust-side rendering architecture — wgpu pipelines, bind groups, vertex buffers, device limits, texture atlases/arrays. NOT the shading math itself (see xindeler-glsl-shading) and NOT terrain worldgen (see xindeler-worldgen).
---

# xindeler-render-architecture

Reference for the `voxygen/src/render/` layer: how pipelines, bind groups, vertex formats and
GPU resources are actually built in this fork. This is `wgpu` (the Rust API is genuinely
`wgpu::*` throughout) — **not WGSL**. Shaders are GLSL 440 compiled via `shaderc`; see
`xindeler-glsl-shading` for that half. A worked example applying everything in this skill is
`docs/design/specs/2026-09-25-pillar2-figure-high-res-shading-texturing.md`.

## Where things live

| Concern | Path |
|---|---|
| Per-draw-type pipelines (figure, terrain, sprite, particle, ui, clouds, ...) | `voxygen/src/render/pipelines/*.rs`, one file per pipeline |
| Shared bind-group layouts (globals, shadow textures, lights) | `voxygen/src/render/pipelines/mod.rs` (`GlobalsLayouts`) |
| Device/adapter creation, feature & limit negotiation | `voxygen/src/render/renderer/mod.rs` |
| Shader source + compile (`shaderc`) | `voxygen/src/render/pipelines/shaders.rs`, sources in `assets/voxygen/shaders/*.glsl` |
| Greedy-mesh quad generation (shared by terrain + figures + sprites) | `voxygen/src/mesh/greedy.rs` |
| Per-draw-type CPU-side meshing (voxel model → `Mesh<Vertex>`) | `voxygen/src/mesh/segment.rs` (figures), `voxygen/src/mesh/terrain.rs` |

## Bind group set convention (figure pipeline; other pipelines follow the same shape)

```
set 0 — global_layout.globals          (camera, sun/moon, tick, focus — shared by everything)
set 1 — global_layout.shadow_textures  (shadow maps — shared)
set 2 — pipeline-specific atlas/material textures (e.g. figure_sprite_atlas_layout)
set 3 — per-instance locals (model matrix, bone data, ...)
```

**`required_limits` in `renderer/mod.rs` uses `..Default::default()` for `max_bind_groups`,
i.e. wgpu's baseline of 4 — and the figure pipeline already uses all 4 with zero headroom.**
Before adding a 5th bind group to any pipeline: check whether the new resource can instead be
folded into the existing per-pipeline `set 2` (extra bindings on the same `BindGroupLayout`,
same `AtlasData`-style trait) — that needs no limits change and stays compatible with every GPU
tier this fork supports (it inherits Veloren's low-end/integrated-GPU support target, not just
the high-end NVIDIA card used as this project's performance baseline). Only bump
`max_bind_groups` deliberately, with an explicit note of which pipelines it affects.

## Vertex format philosophy — bit-packed, not `vec3`/`vec2` fields

Every vertex type in this codebase (`terrain::Vertex`, and any new pipeline-specific vertex type)
packs fields into `u32`s rather than using natural `f32`/`vec3` GPU-side types:

- Position: fixed-point bits within a `u32` (chunk-local range is small and known).
- Normals for voxel faces: **not stored as vectors at all.** Voxel faces are always exactly
  axis-aligned, so only a 2-3 bit axis index + 1 sign bit is stored; the real `vec3` is
  reconstructed in the vertex shader via a lookup into a per-bone/per-model rotation matrix
  (`bones[bone_idx].normals_mat[axis_idx]` in `figure-vert.glsl`). Follow this pattern for any
  new geometric attribute that's naturally low-cardinality per voxel face (e.g. a tangent vector
  is *also* just a constant lookup by the same axis index — no extra vertex bytes needed, see the
  Pillar 2 spec §3.1/§5.1 for the worked derivation).
- Texture/atlas coordinates: packed `u16` pairs into a `u32`, not floats — atlases here are
  small, integer-addressed grids (texel-exact), not arbitrary continuous UV space.

When adding a new vertex attribute, default to this bit-packing style unless there's a specific
reason not to (e.g. a value that's genuinely continuous and doesn't fit in the growing "small
integer index" pattern). `#[repr(C)] #[derive(Copy, Clone, Debug, Zeroable, Pod)]` + a
`wgpu::vertex_attr_array!` of `Uint32`s is the established shape — match it.

## Texture atlas vs. texture array — pick deliberately

Two different existing patterns solve different problems; don't reach for the wrong one:

- **`AtlasData` trait + `VoxelAtlasLayout<T>`** (see `FigureSpriteAtlasData` in `figure.rs`):
  a single, dynamically-packed 2D texture atlas, rebuilt per-model at mesh time via
  `guillotiere`-style rectangle packing. Right for data that's baked per-model-instance (e.g.
  per-voxel lit color) and doesn't tile.
- **`wgpu::TextureViewDimension::D2Array`** (`texture2DArray` in GLSL): a fixed set of
  same-resolution layers, loaded once at asset-load time, indexed by a small integer ID carried
  on the vertex or in a uniform. Right for a shared, engine-lifetime material/texture library
  where every user of layer N gets the exact same texture (tiling materials, not baked-per-model
  data). See the Pillar 2 spec for a full worked design of adding one of these to the figure
  pipeline without exceeding the 4-bind-group ceiling above.

## Shading language — hard rule

Shaders in this repo are **GLSL 440**, compiled to SPIR-V by `shaderc` (the `shaderc-from-source`
cargo feature; see this project's `cargo run` commands in `CLAUDE.md`). There is no `.wgsl` file
anywhere in the tree. `naga` is a dependency of `wgpu` itself (consumes the SPIR-V), not a second
authoring language. Any external document, prompt, or research note that assumes WGSL shader
source for this specific fork is wrong and must be corrected before use — this has already
happened once (see the Pillar 2 spec's §0) and is the canonical example of why source material
must be checked against the live tree before it drives an architecture decision.
