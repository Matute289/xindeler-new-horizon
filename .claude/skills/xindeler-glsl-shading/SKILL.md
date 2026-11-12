---
name: xindeler-glsl-shading
description: Use when writing or changing shader math — lighting/BRDF, materials, texturing, normal/bump mapping — anywhere in assets/voxygen/shaders/*.glsl. NOT the Rust-side pipeline/bind-group/vertex-buffer plumbing (see xindeler-render-architecture).
---

# xindeler-glsl-shading

Reference for `assets/voxygen/shaders/*.glsl` (69 files, `#version 440 core`, compiled via
`shaderc`). **This is GLSL, not WGSL** — see `xindeler-render-architecture` for why that
correction matters and stays load-bearing for any future work here. A worked example applying
everything in this skill is
`docs/design/specs/2026-09-25-pillar2-figure-high-res-shading-texturing.md` (figure normal
mapping + PBR roughness/metallic, grafted onto the existing BRDF below without replacing it).

## Include structure

| File | Role |
|---|---|
| `include/constants.glsl` | `MAT_*` render-material tags (`MAT_SKY`, `MAT_BLOCK`, `MAT_FIGURE`, ...), shared numeric constants |
| `include/globals.glsl` | Per-frame uniforms: camera, sun/moon direction, tick, focus offset |
| `include/light.glsl` | The core lighting entry points — `lights_at`, `get_sun_diffuse2`-adjacent helpers, `light_reflection_factor` (the BRDF itself), `apply_cell_material` (per-voxel special-VFX switch) |
| `include/sky.glsl` | `get_sun_diffuse2`/`get_moon_info`, atmospheric/sky-light contribution |
| `include/srgb.glsl` | Color-space helpers + the greedy-atlas decode functions (`greedy_extract_col_light_figure`, `greedy_extract_col_light_attr`) |
| `include/lod.glsl`, `include/cloud.glsl`, `include/random.glsl` | LOD terrain sampling, volumetric cloud, hash/noise utilities |

Per-pipeline shaders (`figure-vert.glsl`, `figure-frag.glsl`, `terrain-vert.glsl`,
`terrain-frag.glsl`, `sprite-*.glsl`, ...) `#include` these and set `#define`s **before** the
includes to select build variants (see below) — order matters, the includes read the defines.

## The lighting model — already a microfacet BRDF, not a toy diffuse model

Every `*-frag.glsl` that shades a lit surface sets:
```glsl
#define LIGHTING_TYPE LIGHTING_TYPE_REFLECTION
#define LIGHTING_REFLECTION_KIND LIGHTING_REFLECTION_KIND_GLOSSY
#define LIGHTING_DISTRIBUTION_SCHEME LIGHTING_DISTRIBUTION_SCHEME_MICROFACET
#define LIGHTING_DISTRIBUTION LIGHTING_DISTRIBUTION_BECKMANN
```
and computes three reflectance coefficients plus a roughness-like scalar before calling into
`light.glsl`/`sky.glsl`:
```glsl
vec3 k_a = vec3(1.0);   // ambient
vec3 k_d = vec3(1.0);   // diffuse
vec3 k_s = vec3(R_s);   // specular tint — R_s from a fixed-IOR (n2=1.5) Schlick-ish Fresnel term
float alpha = 1.0;      // THIS IS THE BECKMANN ROUGHNESS PARAMETER, not a transparency value
...
get_sun_diffuse2(..., k_a, k_d, k_s, alpha, ...);   // -> light_reflection_factor(...) internally
lights_at(..., k_a, k_d, k_s, alpha, ...);
```
**Before inventing a new BRDF or a new roughness/metallic system for anything, check whether the
existing `k_a`/`k_d`/`k_s`/`alpha` inputs already express what's needed** — in every case found so
far (see the Pillar 2 spec §5.2) they do; the gap has been that `alpha` was hardcoded per
material special-case instead of sampled from real data, not that the BRDF itself was missing a
concept. A metallic workflow grafts onto this cleanly: `k_d = albedo * (1 - metallic)`,
`k_s = mix(vec3(R_s), albedo, metallic)`, `alpha = roughness` — no BRDF-function edits required.

## The existing (tiny) per-voxel material system

`CellSurface` (Rust: `common/src/figure/cell.rs`) is a 5-bit enum baked per-voxel into the col-light
atlas at mesh time, decoded in the fragment shader as `material`/`f_attr`, and dispatched in
`light.glsl`'s `apply_cell_material(material, ...)` — a `switch` with one hardcoded procedural
effect per case (Glowy = emissive boost, Shiny = alpha=0.1 hack + reflection distortion, Fire =
flicker, Water = puddle re-tag, SwirlyCrystal = animated color cycle). This system is **orthogonal**
to per-voxel textures/materials added for real PBR texturing (Pillar 2) — a voxel can be both
`Glowy` and textured; don't conflate the two when extending either.

## `#define`-driven build variants — check before assuming a code path always runs

Shaders branch heavily on preprocessor defines set per pipeline/quality-setting, not runtime
uniforms: `FIGURE_SHADER`, `SHADOW_MODE` (`_NONE`/`_CHEAP`/`_MAP`), `FLUID_MODE` (`_LOW`/`_MEDIUM`+),
`EXPERIMENTAL_*` (`DISCARDTRANSPARENCY`, `BAREMINIMUM`, `NONOISE`, `CURVEDWORLD`, `PHOTOREALISTIC`,
...). New shader code touching lighting must be checked against the relevant `#if`/`#ifdef`
branches — e.g. `EXPERIMENTAL_BAREMINIMUM` skips the entire lighting block and returns
`simple_lighting(...)` directly; code added after the BRDF calls must still make sense (or be
correctly excluded) under that branch.

## Why hard voxel edges survive shading changes

Voxel faces are exactly axis-aligned and greedy-mesh UV/atlas coordinates are always quantized to
voxel-face boundaries (see `xindeler-render-architecture`'s vertex-format section and the Pillar 2
spec §5.3 for the full argument). Any per-fragment shading technique — normal mapping, roughness
sampling, tinting — that only perturbs the *shading* normal/color and never reads or writes vertex
positions cannot soften a cube silhouette, because the silhouette is geometry and this class of
technique never touches geometry. Keep this invariant explicit in any new shader work aimed at
preserving the voxel aesthetic.
