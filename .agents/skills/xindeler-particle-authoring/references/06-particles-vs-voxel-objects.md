# 06 — Particles, voxel objects, and the other four VFX surfaces

This file exists so that "make the fireball effect" lands on the right surface
the first time. It restates the decision made by the sibling skill
`xindeler-voxel-authoring` (and its `voxel-asset-engineer` agent) rather than
merely linking to it, so this skill is self-contained **and remains usable if
that sibling has not landed in your tree yet** — it ships in its own PR.

## The one-sentence rule

> **Author `.vox` for things that are objects — something a player could point
> at and say "that thing is there". Keep particles for things that are
> phenomena — fire, smoke, sparks, light in motion.**

An ice shard is an object; a fireball is a phenomenon. A rune circle is an
object; a poison cloud is a phenomenon. A summoned blade is an object; the
trail it leaves is a phenomenon.

## The six surfaces the engine actually has

| # | Surface | Authored art? | Can it move? | Cost |
|---|---|---|---|---|
| 1 | **Particles** (`ParticleMgr`) | ❌ one shared 1×1×1 voxel; the look is GLSL | ✅ but only by writing GLSL | 56 B/instance, **2 draw calls total**, no global cap |
| 2 | **`Body::Object` entities** | ✅ full `.vox` | ⚠️ 2 rigid bones, animated in Rust | a draw call + bind groups per entity per pass, **plus a network-synced server ECS entity** |
| 3 | **Terrain sprites** | ✅ full `.vox` | ❌ static (wind sway only) | forces a chunk remesh |
| 4 | **Trail pipeline** (`voxygen/src/scene/trail.rs`, `citadel_force_field.rs`) | ❌ procedural mesh | ✅ rebuilt per frame | CPU mesh build |
| 5 | **Point lights** (`LightEmitter`, `Light::new`) | n/a | ✅ flicker/fade | shadow maps, **hard global cap of 20** (`voxygen/src/scene/mod.rs:71`) |
| 6 | **Postprocess** (`last_lightning`) | n/a | ✅ global | full-screen |

**~95 % of spell VFX today is surface 1**, and surface 1 is structurally
incapable of showing authored art. Adding a new particle look is a *shader*
task, not an art task. That is not a limitation to work around — it is what
makes 800 lightning particles free.

## Why not voxelise the phenomena

| Effect | Why `.vox` is worse |
|---|---|
| **Explosions, fireballs** | Stochastic scatter of hundreds of short-lived elements. A voxel mesh is one rigid shape that can only *scale*, so it reads as a growing ball, not an explosion |
| **Lightning, rays, energy bolts** | Already 800 one-shot particles falling up to 600 m, plus a global sky flash. A voxel bolt is a rigid stick |
| **Beams, flamethrowers, breath weapons** | Emitted along a Bezier in a cone at 300–1 600 particles/s, with terrain raycasting to stop at walls. Geometry can do neither the cone spread nor the wall clipping |
| **Poison clouds, smoke, fog, gas** | Volumetric and soft-edged. Voxels are the worst possible representation of a cloud — a rigid green blob |
| **Sparks, embers, blood, dust, motes** | Pure particle work |
| **Buff auras, weapon trails** | Already handled, driven by component state rather than by an entity |

## Why not particle-ise the objects

A `Body::Object` VFX prop is a real, server-side, network-synced ECS entity: it
joins the physics and spatial-grid joins, it replicates to every client in
range, and it needs a `Collider` to be rendered at all. That is several orders
of magnitude more expensive per unit than a particle — so the voxel skill's rule
is **one prop per cast, not one per visual element**. A rune circle is one
entity; forty orbiting motes around it are particles.

But when the ask really is an object, particles cannot fake it: they have no
authored silhouette, no per-model palette, and no shadow-casting geometry. A
"floating crystal focus" built from particles is a cluster of cubes; built as a
`.vox` `Body::Object` it is a crystal.

Go to `xindeler-voxel-authoring` for (or, if that skill is not yet in your tree,
to `Body::Object` + `assets/voxygen/voxel/` + the matching `*_manifest.ron`
directly): projectiles, orbs, wisps, ice
shards/walls, stone spikes, summoned weapons, ground rune circles and sigils,
totems, seals, wards, portals, braziers, and spinning arcane machinery.

## The two that are neither

- **Domes, shields, barriers, translucent walls** → the **trail pipeline**.
  `voxygen/src/scene/citadel_force_field.rs` (Xindeler-authored) builds
  procedural hollow domes as `TrailVertex` quads, and its header explains why a
  sprite cannot do it: the terrain sprite renderer has no per-voxel alpha
  channel, so a glass block is necessarily opaque. Copy that file's approach.
- **A world-wide flash** → the **postprocess path**. `Outcome::Lightning` sets
  `Scene::last_lightning` (`voxygen/src/scene/mod.rs:555`), which drives the sky
  flash (`assets/voxygen/shaders/include/sky.glsl:212`) and a giant transient
  point light (`include/point_glow.glsl:91`). One global uniform, no particles.

## Combining them is the normal answer

Most interesting spell VFX is **both**:

- a conjured ice wall — `.vox` object + frost particles drifting off it;
- an arcane cannon — `.vox` turret + a particle beam (this is exactly what the
  Cromatolis Aerial Citadel ships: two authored `.vox` cannons, with the laser
  and sphere rendered as `CitadelLaser` / `CitadelSphere` particles,
  `particle.rs:805` and `854`);
- a summoning circle — `.vox` sigil on the ground + rising motes.

So when a brief mixes the two, **split it** and say so, rather than forcing all
of it onto one surface. Splitting is the right answer, not a compromise.
