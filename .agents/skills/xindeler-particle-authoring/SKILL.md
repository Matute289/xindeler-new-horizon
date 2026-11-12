---
name: xindeler-particle-authoring
description: Use when authoring a spell or ability VFX that is a *phenomenon* rather than an object — explosions, fire, lightning and beams, poison/acid/gas clouds, sparks, auras, shockwave rings, trails of light — by adding or editing a `ParticleMode` and its GLSL case, and wiring it to the outcome, character state, buff, aura, shockwave or beam that should trigger it. Covers the instance format, the shader's motion/easing library, the six emission entry points, the RON `FrontendSpecifier` surface abilities already use, and the verify loop (there is no offline GLSL compiler here). Not for `.vox` geometry (use `xindeler-voxel-authoring`), not for ability gameplay data (use `xindeler-abilities`).
---

# xindeler-particle-authoring

A particle effect in this engine is **a number and a page of GLSL**. There is no
particle editor, no `.ron` effect file, no curve widget. `ParticleMode` is a
plain Rust enum uploaded per instance as one `i32`, and
`assets/voxygen/shaders/particle-vert.glsl` is a 1,421-line file that is
essentially one giant `switch` on that number (lines 311–1389), computing the
particle's position, size, colour and rotation from its own age. Authoring a new
effect means writing a new `case` in that switch and a new emission site that
spawns instances pointing at it.

That is a smaller and more tractable surface than it sounds: **86 modes are
declared and 85 of them have a shader case** (the odd one out is dead upstream
weight, see `references/05`), and the average case is eight lines of GLSL built
from an existing library of motion functions (`spiral_motion`, `blown_by_wind`,
`slow_start`,
`start_end`, …). The cost is that **every mistake is silent** — wrong number,
missing case, alpha that does nothing, a colour that never glows — so the
discipline in `references/05` is the point of this skill.

## The five facts everything else follows from

All verified by reading the shipped code, not a spec.

1. **Every effect shares one 1×1×1 white voxel and two draw calls.**
   `assets/voxygen/voxel/particle.vox` is a 64-byte file holding a single
   voxel; `ParticleMgr::render` (`voxygen/src/scene/particle.rs:4962`) issues
   exactly two instanced draws — the shared cube, and one Xindeler-added draw
   for citadel spheres using `laser_beam_small.vox`. The *only* per-particle
   art surface is shader math. → `references/01`

2. **The look lives entirely in one `switch`.** Every mode is a `case` in
   `particle-vert.glsl` that fills an `Attr { offs, scale, col, rot }` from
   `lifetime()`, `percent()` and ten hashed random floats. A mode number with no
   `case` falls through to `default:` (`particle-vert.glsl:1378`) and renders as
   generic white motes — **no warning, no log line, no crash**. → `references/02`

3. **`col.a` is a lie; `col.rgb > 1.0` is the glow.** The fragment shader hard-
   codes `alpha = 1.0` and comments out `f_col.a`
   (`assets/voxygen/shaders/particle-frag.glsl:65`), so alpha never fades a
   particle — it only shrinks it, via `attr.scale *= pow(attr.col.a, 0.25)`
   (`particle-vert.glsl:1392`). Emission comes from colour channels *above*
   1.0: `emitted_light += max(f_col.rgb - 1.0, vec3(0))`
   (`particle-frag.glsl:113`). That is why fire is `vec4(6, 3, 0.4, 1)`.
   → `references/02`

4. **Emission has six entry points, and picking the right one is most of the
   design.** One-shot → an `Outcome`; while-casting → `CharacterState`;
   persistent on a target → `Buffs` / `Auras`; expanding ring → `Shockwave`;
   sustained cone/ray → `Beam`; ambient from the world → `BlocksOfInterest`.
   Four of those (`Beam`, `Shockwave`, `Aura`, several `CharacterState`s) are
   selected **from the ability's own RON file** by a `specifier:` field, so a
   new look on an existing mechanic can be pure data. → `references/03`

5. **There is no GLSL compiler on this machine — only the ones the tests
   drive.** Shaders are compiled at runtime, by naga by default and by
   `shaderc` when `VELOREN_DISABLE_NAGA_SHADERS` is set
   (`voxygen/src/render/mod.rs:491`,
   `voxygen/src/render/renderer/pipeline_creation.rs:352`), and hot-reloaded on
   file change (`voxygen/src/render/renderer/mod.rs:1323`); a bad edit logs
   `"Could not recreate shaders from assets due to an error"` (`mod.rs:1297`)
   and keeps the previous pipeline. `cargo test … particle_shaders` drives both
   compilers offline; after that the loop is: run the client and edit the shader
   live. → `references/05`

## Voxel object or particle effect? — decide this first

This skill is the **phenomenon** half of a two-skill pair. Its sibling,
`xindeler-voxel-authoring` (+ the `voxel-asset-engineer` agent), is the
**object** half: `.vox` geometry, palettes, bones and manifests.

> ⚠️ **The sibling ships in its own PR and may not be in your tree yet** —
> check `.claude/skills/xindeler-voxel-authoring/`. If it is missing, do not
> stop: the object surface is `Body::Object` + a `.vox` under
> `assets/voxygen/voxel/` + a row in the matching `*_manifest.ron`, rendered by
> `voxygen/src/scene/figure/`, and `references/06` restates the whole decision
> so nothing here depends on that PR landing.

Its research established the split, and it is restated here so this file stands
on its own:

> **Author `.vox` for things that are objects — something a player could point
> at and say "that thing is there". Keep particles for things that are
> phenomena — fire, smoke, sparks, light in motion.** An ice shard is an
> object; a fireball is a phenomenon. A rune circle is an object; a poison
> cloud is a phenomenon.

| Spell VFX ask | Surface | Skill |
|---|---|---|
| Explosion, fireball, firestorm | particles | **this skill** |
| Lightning bolt, ray, beam, flamethrower, breath weapon | particles | **this skill** |
| Poison / acid / gas cloud, smoke, fog | particles | **this skill** |
| Sparks, embers, blood, dust, motes, buff auras, weapon trails | particles | **this skill** |
| Expanding shockwave ring on the ground | particles | **this skill** |
| Projectiles, orbs, wisps, ice shards, summoned weapons | `.vox` `Body::Object` | `xindeler-voxel-authoring` |
| Ground rune circles / sigils, totems, seals, wards, portals | `.vox` `Body::Object` | `xindeler-voxel-authoring` |
| Spinning arcane machinery, braziers | `.vox`, 2-bone object | `xindeler-voxel-authoring` |
| Translucent domes, shields, barriers | neither — the trail pipeline, `voxygen/src/scene/citadel_force_field.rs` | see `references/06` |

The two are routinely **combined**: a conjured ice wall is a `.vox` object that
emits frost particles. Splitting the ask that way is usually the right answer,
not a compromise. Full reasoning, including why a voxel explosion reads as a
growing ball and why a `Body::Object` costs orders of magnitude more per unit
than a particle, is in `references/06`.

## Read first

Each reference is short and self-contained; read the one that covers your task.
They live in the repo, beside this file, because they describe public engine
code — a worktree session must be able to read them without a private-repo pull.

- `references/01-pipeline-and-instance-format.md` — the render pipeline, the
  56-byte instance, the three constructors and what each field really means,
  the blend/depth state, the shared model and the one exception to it.
- `references/02-shader-authoring.md` — **read before writing a single line of
  GLSL.** The `Attr` contract, the full motion/easing function library, the
  emissive-colour rule, `f_reflect`, entropy, time-overflow safety, and the
  five shader traps.
- `references/03-emission-and-triggers.md` — the six entry points with the
  decision table, `HeartbeatScheduler`, `add_particles` vs `push_particle`,
  `particles_chance`, and the RON `specifier:` surface that lets a new look ship
  as data.
- `references/04-shipped-effect-catalog.md` — every declared mode grouped by
  phenomenon, with the reusable idiom each one demonstrates. Start here when
  the ask is "make a poison cloud" — something close probably already exists.
- `references/05-authoring-workflow.md` — the step-by-step recipe, the
  verification loop, the performance budget, and what only a human can check.
- `references/06-particles-vs-voxel-objects.md` — the full decision rationale
  and the other VFX surfaces (trail pipeline, point lights, the sky flash).

Two checkers back this up, with different jobs:

- `tools/particles/particle_modes.py` (no dependencies, Python 3.9+) —
  cross-checks the Rust enum against the GLSL in under a second with no build,
  and prints the next free mode number. **This is the one CI runs**
  (`.github/workflows/ci-code-quality.yml`), so it is what actually guards the
  invariant on a PR.
- `shader_modes_match_particle_modes` and the two `particle_shaders_*` tests in
  `voxygen/src/render/pipelines/particle.rs` — the deeper gate: they enumerate
  the real enum, check `break;`s and the fragment shader's own copy of a mode
  number, and compile the GLSL through both of the renderer's compilers. CI
  compiles them (clippy `--all-targets`) but **does not run them** — running
  `cargo test` is on you.

## Workflow

1. **Check it is a particle problem** (table above, `references/06`), and
   **check it does not already exist** (`references/04` — 86 modes are declared, and
   several are close to what a new spell needs).
2. **Try data first.** If the mechanic is a beam, shockwave, aura or a
   `CharacterState` with a `FrontendSpecifier`, an existing look may be one RON
   line away (`specifier: Poison`). Zero Rust, zero GLSL. → `references/03`
3. **Add the mode**: a variant appended at the end of `ParticleMode` with the
   next free number (`python3 tools/particles/particle_modes.py` prints it), and
   a matching `const int` + `case` in `particle-vert.glsl`.
4. **Write the emission site** in `voxygen/src/scene/particle.rs` at whichever
   of the six entry points matches the trigger, honouring
   `scene_data.particles_chance` and the `HeartbeatScheduler`.
5. **Verify**: `python3 tools/particles/particle_modes.py`, then
   `cargo test -p xindeler-voxygen --no-default-features --features
   shaderc-from-source particle`, then run the client and trigger it —
   `/outcome` fires any `Outcome` variant on demand, and the shader hot-reloads
   while the client runs, which is the real iteration loop.
6. **Ship it**: branch off the current working branch, one PR, run the
   specialist reviewers against your own diff, never merge.

## Non-negotiables

- **Never reuse a mode number.** The Rust discriminant and the GLSL `const int`
  are bound by value only; reusing one silently repaints a shipped effect.
  Append at the end, use the number the checker prints.
- **Never rely on `col.a` to fade a particle.** It does nothing but shrink it.
  Fade by driving `col.rgb` toward zero with `start_end(1.0, 0.0)`.
- **Never write a `default:`-reachable mode.** A mode without a `case` is not a
  "fallback", it is an invisible bug; the checker and the Rust test exist
  precisely because nothing else catches it.
- **Budget by `count × lifespan`, not by cost per particle.** A particle costs
  no CPU after spawn, but it is 72 B resident and the whole pool is re-collected
  into fresh `Vec`s and fresh GPU buffers every frame (`references/01`). And
  emission rates are real: a single `Cultist` beam emits **960 particles/second**
  (`particle.rs:2788`) and one lightning strike spawns **800 at once**
  (`particle.rs:95`). Scale with the spell, and multiply by
  `scene_data.particles_chance` unless you are drawing exactly one deterministic
  visual per entity per frame — players on Low run at 0.1.
- **`Light::new` from a particle maintainer evicts other lights.**
  `MAX_LIGHT_COUNT = 20` (`voxygen/src/scene/mod.rs:71`), and the scene sorts
  every light by distance to the viewpoint and truncates
  (`voxygen/src/scene/mod.rs:976`) — so a light near the camera does not fail to
  fit, it pops nearby torches and campfires off for as long as it lives. Gate it
  on `scene_data.flashing_lights_enabled` as the beam code does — that flag is
  an accessibility setting, not a quality setting.
- **Particles write depth and are effectively opaque.** Do not design an effect
  that depends on blending through another particle. (`WATER_FOAM` is the single
  hardcoded exception — `references/01`.)
