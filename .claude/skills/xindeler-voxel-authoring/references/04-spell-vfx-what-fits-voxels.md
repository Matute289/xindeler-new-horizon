# 04 — Spell VFX: what actually belongs in a `.vox`

The request behind this skill was *"hand-authored `.vox` for spell effects —
colours, explosions, lightning, poison clouds, everything the spell
descriptions mention."* Half of that is a good fit and half of it is a
regression. This file says which is which, and why, with the engine's real
constraints rather than a preference.

## The six surfaces the engine has

| # | Surface | Can you author art? | Can it move? | Cost |
|---|---|---|---|---|
| 1 | **Particles** (`ParticleMgr`) | ❌ every particle is the same 1×1×1 voxel | ✅ but only by writing GLSL | 56 B/instance, 2 draw calls, **no cap** |
| 2 | **`Body::Object` entities** | ✅ **full `.vox`** | ⚠️ 2 rigid bones, animated in Rust | one figure entity |
| 3 | **Terrain sprites** | ✅ full `.vox` | ❌ static (wind-sway only) | forces a chunk remesh |
| 4 | **Trail pipeline** (`trail.rs`, `citadel_force_field.rs`) | ❌ procedural mesh | ✅ rebuilt per frame | CPU mesh build |
| 5 | **Point lights** (`EventLight`, `LightEmitter`) | n/a | ✅ flicker/fade | shadow-map cost |
| 6 | **Postprocess** (`last_lightning`) | n/a | ✅ global | full-screen |

**The blunt version:** ~95% of spell VFX today is path 1, and path 1 is
structurally incapable of showing authored art — `assets/voxygen/voxel/particle.vox`
is a single white voxel and every one of the 86 `ParticleMode`s is a hardcoded
`case` in a 1,420-line GLSL switch. Adding a new particle look is a *shader*
task, not an art task.

The mechanism that does show authored art is **path 2, `Body::Object`**, and
it is already proven in this repo: the Cromatolis Aerial Citadel (COW-8)
shipped five visual `Body::Object` variants three weeks ago, two of them with
new hand-made `.vox`.

## The `Body::Object` recipe

What you get: a `.vox` model (or two, one per bone), real lighting and
shadows, `visual_scale()` for display-only size, `custom_indices` for
Fire/Water/SwirlyCrystal surfaces, an optional `LightEmitter`, and
`Object::DeleteAfter { spawned_at, timeout }` for a self-expiring effect.

What it takes:

1. A variant in `common/src/comp/body/object.rs` with the next free
   discriminant, plus arms in `to_string()`, `density()`, `mass()`,
   `dimensions()`, optionally `visual_scale()`.
2. The `.vox` under `assets/voxygen/voxel/object/`.
3. A `bone0`/`bone1` row in `assets/voxygen/voxel/object_manifest.ron`
   (unused bone → `central: ("armor.empty")`).
4. Spawn it server-side with `create_object` + `Pos`/`Ori`/`Body`, add
   `Object::DeleteAfter` for a timed effect, `LightEmitter` for glow
   (`NapalmPool` in `server/src/state_ext.rs:459-491` is the working template
   for a ground-attached, timed, flickering effect object).

Animation available out of the box: `ObjectSkeleton` has exactly **2 bones**
and four animations — `idle`, `shoot`, `beam`, `turret`
(`voxygen/anim/src/object/`). `turret.rs` is Xindeler's, driven by
server-synced `CitadelTurretAngles`. Anything beyond "spin, pulse, or
two-part articulation" means writing a new `impl Animation` in
`voxygen/anim/src/object/`.

**The hard constraint, stated once:** there is **no multi-frame `.vox`
animation anywhere in this engine**. You cannot author frame 1, frame 2,
frame 3 of an unfurling glyph. Motion is either bone transforms in Rust, or a
time-varying shader surface (`Fire` / `SwirlyCrystal`, reference 02), or
particles.

## Verdict per effect category

### ✅ Good `.vox` candidates — author these

| Effect | Why | Notes |
|---|---|---|
| **Ground AoE rune circles / sigils** | Flat, geometric, symmetric — exactly what parametric generation is for, and radius/spoke-count can come from the spell's own numbers | The engine *asks for this*: `voxygen/src/scene/particle.rs:2362` carries `TODO(magic-v1 polish): dedicated decal/ParticleMode; CultistFlame is a readable placeholder ring`. Also replaces the shockwave path's per-particle terrain raycasting (~2 µs each, measured peak 113/tick) |
| **Projectiles** (bolts, shards, orbs, spectral blades) | Zero new machinery: a projectile already *is* a `Body::Object` with a `.vox` | 43 such models already ship under `assets/voxygen/voxel/weapon/projectile/` |
| **Floating orbs, foci, wisps, motes** | Small, round, glowy — `Palette.add(col, GLOWY)` or `custom_indices` `SwirlyCrystal` gives live shimmer for one RON line | `Object::FloatingDisk` already exists as a hovering prop |
| **Solid conjurations** — ice shards/walls, stone spikes, thorn barriers, summoned weapons | Genuinely solid geometry that should cast shadows and read as an object | |
| **Totems, seals, wards, portals, braziers** | Persistent props; `portal.vox` and `gnarling_totem_*.vox` are the existing pattern | |
| **Spinning/pulsing arcane machinery** | Two-bone articulation is enough | The citadel cannon is exactly this |
| **`PhantasmDissipated`** | A Xindeler outcome (`common/src/outcome.rs:91`) that fires, carries `pos` *and* `body`, and **currently renders nothing** (`particle.rs:745` catch-all) | A ready-made hook |

### ❌ Keep as particles — do not voxelise

| Effect | Why not |
|---|---|
| **Explosions, fireballs** | Stochastic scatter of hundreds of short-lived elements. A voxel mesh gives you one rigid shape that must expand by scaling — it reads as a growing ball, not an explosion. Already `60–75 × power` instanced particles at 56 B each |
| **Lightning / rays / bolts of energy** | Already 800 one-shot particles falling from up to 600 m, *plus* a global sky flash via `last_lightning`. A voxel bolt would be a rigid stick |
| **Beams, flamethrowers, breath weapons** | Emitted along a Bezier in a cone at 300–1,600 particles/s with terrain raycasting to stop at walls. Geometry can't do the cone spread or the wall clipping |
| **Poison clouds, smoke, fog, gas** | Volumetric and soft-edged. Voxels are the worst possible representation of a cloud — you'd get a rigid green blob |
| **Sparks, embers, blood, dust, motes in motion** | Pure particle work |
| **Buff/aura auras, weapon trails** | Already handled, and driven by component state rather than by an entity |

### 🟡 Neither — use the trail pipeline

Domes, shields, barriers, translucent walls. `voxygen/src/scene/citadel_force_field.rs`
(Xindeler-authored) builds procedural hollow domes as `TrailVertex` quads and
its header explains why sprites can't do it: *the terrain sprite renderer has
no per-voxel alpha channel, so a glass block is necessarily opaque*. Copy that
file's approach, don't author a `.vox`.

## The honest split, in one sentence

**Author `.vox` for things that are objects — something a player could point
at and say "that thing is there". Keep particles for things that are
phenomena — fire, smoke, sparks, light in motion.** An ice shard is an object;
a fireball is a phenomenon. A rune circle is an object; a poison cloud is a
phenomenon.

## Cheap wins before you write any Rust

In rough order of effort:

1. **A `custom_indices` line on an existing model.** `Fire` and
   `SwirlyCrystal` animate in the shader for free. A static crystal `.vox`
   becomes a living arcane focus with three lines of RON.
2. **A new `Body::Object` reusing an existing mesh.** The citadel's three
   laser bodies did exactly this — new body, new size/mass, existing `.vox`.
3. **A new `.vox` on an existing 2-bone object.** Full authored art, no new
   Rust beyond the `Body` variant arms.
4. **A new object animation** in `voxygen/anim/src/object/`. Only when a spin
   or a pulse genuinely isn't enough.

## Where a VFX asset goes

There is **no `assets/voxygen/voxel/vfx/` directory today** and no VFX asset
category at all — the ~60 magic-ish models in the repo are all projectiles or
props. If this becomes a real content pillar, propose the directory in the
same PR that adds the first few assets, and put it in
`assets/voxygen/voxel/README.md` alongside the existing category list.
