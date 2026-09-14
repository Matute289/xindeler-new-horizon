# 02 — Shader authoring: the `Attr` contract and the motion library

**Read this before writing a single line of GLSL.** Everything visible about a
particle is decided here, in `assets/voxygen/shaders/particle-vert.glsl`.

## The contract

Every `case` in the mode switch does exactly one thing: fill an `Attr`
(`particle-vert.glsl:137`).

```glsl
struct Attr {
    vec3 offs;   // offset from inst_pos, in world units
    vec3 scale;  // size, in units of 1/11 of a block (SCALE, line 38)
    vec4 col;    // rgb: linear colour, >1.0 glows.  a: shrink factor, NOT opacity
    mat4 rot;    // rotation applied to the cube and to its normals
};
```

After the switch, three lines do all the remaining work
(`particle-vert.glsl:1392`):

```glsl
attr.scale *= pow(attr.col.a, 0.25);                             // alpha shrinks
f_pos = start_pos + (v_pos * attr.scale * SCALE * mat3(attr.rot) + attr.offs);
f_col = vec4(attr.col.rgb, attr.col.a);
```

`SCALE = 1.0 / 11.0` (`particle-vert.glsl:38`), so `attr.scale` of `1.0` is
roughly a 9 cm cube and the common `vec3(3.0)` is roughly 27 cm. `start_pos` is
`inst_pos - focus_off.xyz` (`particle-vert.glsl:306`) — the renderer's floating
origin; never use `inst_pos` directly.

`gl_Position = all_mat * vec4(f_pos, 1)`. There is no per-particle
billboarding: a particle is a rotated cube in world space, not a camera-facing
quad. `spin_in_axis(axis, angle)` on random axes is what makes small cubes read
as amorphous specks.

## Your inputs

| Input | Source | Notes |
|---|---|---|
| `lifetime()` | `time_since(inst_time)` | seconds since spawn, overflow-safe |
| `percent()` | `lifetime() / inst_lifespan` | 0 → 1 over the particle's life |
| `inst_dir` | the instance | direction **or** colour — see `references/01` |
| `inst_start_wind_vel` | the instance | wind at spawn, for `blown_by_wind()` |
| `rand0 … rand9` | `hash(vec4(inst_entropy + N))` | ten stable per-particle floats |
| `tick.x`, `tick_loop(...)` | globals | wall-clock time, for effects that pulse in phase |
| `focus_pos`, `cam_pos` | globals | `PORTAL_FIZZ` uses `focus_pos` to colour by view angle |

`rand0..rand9` are computed unconditionally at the top of `main`
(`particle-vert.glsl:295`) — use them freely, they are already paid for. They
are *stable for the particle's whole life*, which is what lets a case give each
particle its own speed, hue jitter and spin without any state.

## The motion and easing library

These already exist; compose them instead of writing new math.

### Position

| Function | Line | Shape |
|---|---|---|
| `linear_motion(init_offs, vel)` | 156 | constant velocity from an offset |
| `grav_vel(g)` | 174 | add to a velocity for a ballistic arc (`earth_gravity = 9.807`) |
| `on_floor(z, bounce, pos)` | 160 | clamps to a height with a damped bounce — `SHRAPNEL`, `DUST` |
| `quadratic_bezier_motion(start, ctrl0, end)` | 168 | curved flight |
| `spiral_motion(line, radius, t, freq, offset)` | 274 | helix along an arbitrary axis — the workhorse for tornadoes, healing swirls, lifesteal beams |
| `blown_by_wind(mass, drift)` | 284 | wind drift plus a layered sine wobble; heavier `mass` = later onset |

### Easing (all take/return 0→1 over the life)

| Function | Line | Curve |
|---|---|---|
| `percent()` | 186 | linear |
| `slow_start(factor)` | 194 | eases in — small `factor` = sharper ramp |
| `slow_end(factor)` | 190 | eases out |
| `start_end(from, to)` | 198 | linear interpolation between two values |
| `linear_scale(f)` / `exp_scale(f)` | 182 / 178 | growth driven by raw lifetime |

The idiom `vec3(3.0 * (1.0 - slow_start(0.2)))` (used by ~20 modes) means
"start at full size, shrink with an ease" — it is the default puff.

### Rotation

| Function | Line | Use |
|---|---|---|
| `spin_in_axis(axis, angle)` | 202 | tumble; pass random axes + `rand9 * 3 + lifetime() * k` |
| `align_to_axis(axis)` | 229 | point the cube's long side down a vector — sparks, arcs |
| `identity()` | 251 | no rotation; `LIGHTNING` uses this deliberately |
| `perp_axis1` / `perp_axis2` | 260 / 264 | build a frame from one vector |

The beam idiom is `spin_in_axis(normalize(cross(inst_dir, vec3(0,0,1))),
asin(inst_dir.z / length(inst_dir)) + PI / 2.0)` with
`scale = vec3(1.0, 1.0, 50.0)` — a long thin stroke laid along `inst_dir`
(`WEB_STRAND`, line 716; `LASER`, 632; `CITADEL_LASER`, 1344).

### Time that survives the overflow

`inst_time` wraps every `TIME_OVERFLOW = 300 000` seconds
(`voxygen/src/render/pipelines/mod.rs:99`). Use `lifetime()` /
`time_since()`, which handle the wrap (`include/globals.glsl:66`), and
`loop_inst_time(period, scale)` (`particle-vert.glsl:148`) or
`tick_loop(...)` for anything that pulses on wall-clock time. **Never compute
`tick.x - inst_time` by hand** — it goes hugely negative once every 3.5 days of
uptime and the effect visibly breaks for everyone at once.

## Colour

Linear, not sRGB, and unbounded above:

- **Below 1.0** — ordinary diffuse. `POISON` is `vec3(0.3, 0.7, 0.37) * (1.9 +
  rand5 * 0.3)` (line 1124): a muted green that still lands slightly above 1.0
  and so glows a little.
- **Above 1.0** — emissive, by exactly the excess
  (`particle-frag.glsl:113`). `CAMPFIRE` is `vec4(10, 3 + …, 0.4, 1)` (line 342)
  — a red channel ten times "white". Magic modes routinely sit at 5–25.
- **Set `f_reflect = 0.0`** for anything emissive, or the world's lighting will
  also modulate it and it will change colour between day and night.
- **Vary per particle** with a `rand*` term; a fixed colour across hundreds of
  instances reads as flat plastic. Every shipped fire mode jitters green by
  `rand5 * 0.3`.
- **Vary over the life** with `mix(a, b, percent())` or by subtracting
  `0.8 * percent()` from a channel — the shipped fires cool from yellow to red
  that way.

## The five traps

1. **`col.a` is not opacity.** It only shrinks (`line 1392`), because the
   fragment shader forces `alpha = 1.0` (`particle-frag.glsl:65`). Fade with
   `rgb`.
2. **A missing `case` is invisible, not loud.** Fall-through to `default:`
   (line 1378) gives generic white upward motes. Nothing logs. This is what
   `tools/particles/particle_modes.py` and the
   `shader_modes_match_particle_modes` test exist to catch.
3. **GLSL `switch` cases share one scope.** Shipped cases rely on this —
   `momentum`, `vel`, `perp_axis`, `spiral_radius` are declared in one case and
   *reused without redeclaration* in later ones (e.g. `spiral_radius` declared
   at line 514, reused at 536 and 708). So: declaring a new variable in your
   case can collide with an existing name, and copying a case to a new position
   can break it by moving it above its declaration. Prefer a distinctly named
   local (`float miasma_col = …`), and if you move a case, compile before you
   trust it.
4. **`inst_dir` means whatever the emitter meant.** Check which constructor the
   emission site calls before reading it as a direction (`references/01`).
5. **Reusing a mode number silently repaints a shipped effect.** The Rust
   discriminant and the GLSL `const int` are bound by value alone.

## Compiling it

There is no `glslc`, `glslangValidator` or `naga` binary on this machine, and
the client only compiles shaders at runtime. Instead:

```bash
VELOREN_ASSETS="$(pwd)/assets" \
  cargo test -p xindeler-voxygen --no-default-features \
  --features shaderc-from-source particle_shaders
```

runs both particle shaders through **both** compilers the renderer can use, and
takes well under a second once `voxygen` is built:

- `particle_shaders_compile` drives `shaderc` — the renderer's *fallback* path —
  with the same forced `430 core` profile and the same include whitelist. It is
  the stricter of the two and it reports the **exact line number** of a syntax
  error, so this is the one whose output you read.
- `particle_shaders_parse_with_naga` drives naga's GLSL frontend, which is the
  renderer's **default** (`PipelineModes::enable_naga` is on unless
  `VELOREN_DISABLE_NAGA_SHADERS` is set,
  `voxygen/src/render/mod.rs:491`, honoured at
  `voxygen/src/render/renderer/pipeline_creation.rs:352`). The two accept
  slightly different GLSL, so passing only the shaderc one is not proof the
  client will load your shader.

Both are a syntax and linkage gate for **one define configuration**: neither
compiles `particle-vert.glsl`'s `#ifdef EXPERIMENTAL_CURVEDWORLD` arm (line
1396) nor `particle-frag.glsl`'s `#ifdef EXPERIMENTAL_BAREMINIMUM` arm (line
40), and neither says anything about how the effect looks.

While the client is running, saving `particle-vert.glsl` triggers a live
pipeline rebuild (`voxygen/src/render/renderer/mod.rs:1323`); a compile failure
logs `"Could not recreate shaders from assets due to an error"`
(`mod.rs:1297`) and keeps the previous pipeline, so the client does not crash
and you can keep editing. That is the real iteration loop.
