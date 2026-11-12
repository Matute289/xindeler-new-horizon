# 01 — The pipeline and the 56-byte instance

Everything a particle *is* on the CPU side. Read this before you touch
`ParticleMgr`, and `references/02` before you touch the shader.

## The whole render path, in one paragraph

`ParticleMgr` (`voxygen/src/scene/particle.rs:40`) keeps a `Vec<Particle>` of
live particles. Once per frame `maintain` (`particle.rs:749`) drops the expired
ones, runs fourteen `maintain_*` emitters that push new ones, and uploads the
whole vector to a fresh GPU instance buffer (`upload_particles`,
`particle.rs:4941`). `render` (`particle.rs:4962`) then issues **up to two**
instanced draws: the shared particle cube with every ordinary instance, and
`laser_beam_small.vox` with the citadel-sphere instances. (`ParticleDrawer::draw`
early-returns on an empty instance buffer — `voxygen/src/render/renderer/drawer.rs:1305`
— so with no citadel sphere active, which is nearly always, it is one draw.)
That is the entire per-frame cost of every particle in the game.

## The model: one white voxel, shared by all 86 modes

`DEFAULT_MODEL_KEY = "voxygen.voxel.particle"` (`particle.rs:4995`) resolves to
`assets/voxygen/voxel/particle.vox` — a **64-byte file containing a single
1×1×1 voxel**. `default_cache` (`particle.rs:5001`) meshes it once, recentres
its vertices on the origin (`particle.rs:5018`), and caches the model.

So a particle is a tiny cube. Its *apparent* shape comes entirely from the
shader's per-instance scale and rotation: a beam stroke is that cube scaled to
`vec3(1.0, 1.0, 50.0)` and rotated onto the beam axis (`WEB_STRAND`,
`particle-vert.glsl:716`); a spark is it scaled to `vec3(len, 0.15, 0.15)` and
aligned to its direction (`ELECTRIC_SPARKS`, `particle-vert.glsl:1282`).

**The one exception** is Xindeler's own `CITADEL_SPHERE_MODEL_KEY`
(`particle.rs:4999`), which draws a second instance buffer with an actual `.vox`
projectile model. That is the escape hatch when a *single* moving object needs
real geometry but not a full ECS entity — but it costs a second draw call and a
second model, so adding a third is a design decision, not a default.

## `Instance` — the 56 bytes you get to fill

`voxygen/src/render/pipelines/particle.rs:161`:

| Field | Bytes | Shader name | Meaning |
|---|---|---|---|
| `inst_time` | 4 | `inst_time` | spawn time, wrapped by `TIME_OVERFLOW` (300 000 s) |
| `inst_lifespan` | 4 | `inst_lifespan` | total life in seconds; the denominator of `percent()` |
| `inst_entropy` | 4 | `inst_entropy` | per-particle random seed; hashed into `rand0..rand9` |
| `inst_mode` | 4 | `inst_mode` | the `ParticleMode` number the shader switches on |
| `inst_dir_color` | 12 | `inst_dir` | **direction *or* colour** — see below |
| `inst_pos` | 12 | `inst_pos` | world-space spawn position |
| `inst_start_wind_vel` | 8 | `inst_start_wind_vel` | wind at spawn, for `blown_by_wind()` |
| `inst_voxel_light` | 8 | `inst_voxel_light` | `(sunlight, glow)` sampled at spawn |

56 bytes on the GPU, `VertexStepMode::Instance`, eight vertex attributes at
locations 2–9 (`particle.rs:273`). The CPU-side element is larger — see the
cost model at the end of this file. There is **no per-particle size, no per-particle
lifetime curve, no texture, no user data**. Anything else you want the shader
to know has to be smuggled through `inst_dir_color`, derived from
`inst_entropy`, or added as a new instance field (which costs bytes on *every*
particle in the game, so justify it).

### The `inst_dir_color` overload — the field that trips people up

One 12-byte slot serves two incompatible purposes, chosen by which constructor
you call:

- `Instance::new` (`particle.rs:203`) → `[0.0, 0.0, 0.0]`. The shader gets
  nothing; motion must come from `inst_entropy` alone.
- `Instance::new_directed` (`particle.rs:223`) → `pos2 - pos1`. The shader gets
  a **vector**: its length is the travel distance and its direction the aim.
  This is how beams, sparks, explosions and lightning are aimed.
- `Instance::new_colored` (`particle.rs:244`) → the `Rgb<f32>` you pass. The
  shader gets a **colour**, and cannot also be directed.

A mode's `case` must agree with the constructor its emitters use. Reading
`inst_dir` as a direction in a mode that is only ever spawned `new_colored`
gives you particles that fly off toward the colour — a real class of bug, and
the reason `DUST` (`particle-vert.glsl:1317`) explicitly does
`srgb_to_linear(inst_dir)`: it is a colour there, and the comment in the struct
(`particle.rs:177`) is the only documentation of that anywhere.

`with_light` (`particle.rs:265`) is a separate builder applied afterwards; the
default `(1.0, 0.0)` means "fully sunlit, no glow", which is wrong indoors.
`maintain_char_state_particles` passes the caster's own sampled light
(`particle.rs:1835`) — copy that when your effect can happen in a dungeon.

## Blend, depth, and why alpha does not work

`ParticlePipeline::new` (`particle.rs:307`) sets `SrcAlpha / OneMinusSrcAlpha`
blending and `depth_write_enabled: true` with `CompareFunction::GreaterEqual`
(reverse-Z). That looks like ordinary alpha blending — but the fragment shader
hardcodes `float alpha = 1.0/*f_col.a*/;`
(`assets/voxygen/shaders/particle-frag.glsl:65`), with a comment two lines above
the write: *"Temporarily disable particle transparency to avoid artifacts"*
(`particle-frag.glsl:125`).

Consequences you must design around:

1. **Particles are opaque and write depth.** They occlude each other and
   everything behind them. A "faint haze" cannot be built out of many
   low-alpha particles. The one shipped exception is `WATER_FOAM`, which the
   fragment shader *does* give `alpha = 0.5` (`particle-frag.glsl:120`) — so
   translucency is reachable, but only by adding a mode to that same hardcoded
   `f_mode` branch, deliberately, and with the depth-ordering consequences
   that blending unsorted particles implies.
2. **`col.a` still has an effect, but not the one you expect.** The vertex
   shader does `attr.scale *= pow(attr.col.a, 0.25)` (`particle-vert.glsl:1392`),
   so alpha *shrinks*. That is the engine's stated substitute for fading, and
   it is why so many cases end with `start_end(1.0, 0.0)` in the alpha slot.
3. **To fade out, drive `col.rgb` toward zero**, not `col.a`. `SMOKE`
   (`particle-vert.glsl:312`) does both: colour times `start_end(1.0, 0.0)` in
   alpha for the shrink, and a dim base colour so it dies dark.

The second render target (`Rgba8Uint`, `particle.rs:384`) is the material/normal
buffer; the fragment shader writes `MAT_BLOCK` for everything except
`WATER_FOAM`, which writes `MAT_PUDDLE` (`particle-frag.glsl:117`). If your
effect needs a different deferred material, that hardcoded `f_mode` comparison
is where it goes — and note it is the *only* place the fragment shader looks at
the mode at all.

## Lighting

Particles are lit like world geometry: `get_sun_diffuse2` plus `lights_at`
(`particle-frag.glsl:101`), modulated by the `inst_voxel_light` sample and a
shadow lookup. Two knobs matter to an author:

- **`f_reflect`**, set per-case in the vertex shader. `1.0` (the default) means
  the particle is lit by the world; `0.0` means it ignores lighting entirely —
  used by every fire, magic and energy mode, with the recurring comment *"Fire
  doesn't reflect light, it emits it"* (`particle-vert.glsl:335`). Any spell
  effect that should read the same at noon and in a cave sets `f_reflect = 0.0`.
- **Emission through over-bright colour**: `emitted_light += max(f_col.rgb -
  1.0, vec3(0))` (`particle-frag.glsl:113`). Channels above 1.0 glow; below 1.0
  they are ordinary diffuse. This is the whole HDR story — there is no separate
  emission field.

## Cost model

Per particle there is **no CPU work after spawn**: all motion is recomputed in
the vertex shader from `inst_time` every frame, nothing is simulated on the CPU,
nothing is written back. What a live particle does cost, per frame, is more than
the 56-byte instance suggests:

- **72 bytes resident, 56 bytes uploaded.** The GPU instance is 56 B; the
  CPU-side `Particle` (`particle.rs:5106`) is `f64 alive_until` + that instance
  + `Option<Entity>` = **72 B**, align 8.
- **Two `Vec`s collected and two fresh GPU buffers allocated, every frame.**
  `upload_particles` (`particle.rs:4941`) `collect()`s both particle pools into
  new `Vec<ParticleInstance>`s and calls `renderer.create_instances` twice; each
  call is `Instances::new` → `DynamicBuffer::new` → a brand-new `wgpu` buffer,
  with the previous frame's dropped. That is what the function's own
  `// TODO: optimise buffer writes` is about, and it is the single largest
  per-frame particle cost.
- **Two full O(n) scans of the shared pool per frame**, not one: the expiry
  `retain` at `particle.rs:763`, and
  `retain(|p| p.citadel_beam_entity.is_none())` at `particle.rs:821`, which runs
  **unconditionally — even with no citadel beam anywhere in the world**.
- **There is no global particle cap.** `add_particles` (`particle.rs:900`)
  `resize_with`s the vector by however many you ask for. The only throttle is
  `scene_data.particles_chance` and whatever the emitter itself does. Emission
  rate is therefore the author's responsibility — see `references/03`.

So sizing a budget: 20 000 live particles is ~1.4 MB resident, plus ~1.1 MB of
transient `Vec` and ~1.1 MB of freshly-allocated GPU buffer *per frame*, plus
two 20 000-element scans and a 20 000-element copy. Still affordable — but it is
`count × lifespan` that drives it, not the spawn burst, and it is a different
order of decision from "56 bytes, therefore free".
