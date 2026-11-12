# 04 — Spell VFX: what actually belongs in a `.vox`

The request behind this skill was *"hand-authored `.vox` for spell effects —
colours, explosions, lightning, poison clouds, everything the spell
descriptions mention."* Half of that is a good fit and half of it is a
regression. This file says which is which, and why, with the engine's real
constraints rather than a preference.

## The six surfaces the engine has

| # | Surface | Can you author art? | Can it move? | Cost |
|---|---|---|---|---|
| 1 | **Particles** (`ParticleMgr`) | ❌ every particle is the same 1×1×1 voxel | ✅ but only by writing GLSL | 56 B/instance, **2 draw calls total**, no global count cap |
| 2 | **`Body::Object` entities** | ✅ **full `.vox`** | ⚠️ 2 rigid bones, animated in Rust | a draw call + bind groups **per entity per pass**, plus a synced server ECS entity |
| 3 | **Terrain sprites** | ✅ full `.vox` | ❌ static (wind-sway only) | forces a chunk remesh |
| 4 | **Trail pipeline** (`trail.rs`, `citadel_force_field.rs`) | ❌ procedural mesh | ✅ rebuilt per frame | CPU mesh build |
| 5 | **Point lights** (`EventLight`, `LightEmitter`) | n/a | ✅ flicker/fade | shadow maps, and a hard global cap of 20 |
| 6 | **Postprocess** (`last_lightning`) | n/a | ✅ global | full-screen |

**The blunt version:** ~95% of spell VFX today is path 1, and path 1 is
structurally incapable of showing authored art — `assets/voxygen/voxel/particle.vox`
is a single white voxel and every one of the 86 `ParticleMode`s is a hardcoded
`case` in a 1,420-line GLSL switch. Adding a new particle look is a *shader*
task, not an art task.

The mechanism that does show authored art is **path 2, `Body::Object`**, and
it is already proven in this repo: the Cromatolis Aerial Citadel (COW-8)
added five `Body::Object` variants three weeks ago. Read that precedent
precisely, though — only the **two cannons** are actually spawned as figure
entities (and only they got new hand-made `.vox`); the three laser bodies
exist as size/`Outcome` tokens whose visible form is rendered as *particles*.
So the proof-of-concept is two authored props plus an argument for the
timeline pattern described below.

## The `Body::Object` recipe

What you get: a `.vox` model (or two, one per bone), real lighting and
shadows, `visual_scale()` for display-only size, `custom_indices` for
Fire/Water/SwirlyCrystal surfaces, an optional `LightEmitter`, and
`Object::DeleteAfter { spawned_at, timeout }` for a self-expiring effect.

What it takes:

1. A variant in `common/src/comp/body/object.rs`, **appended at the end** of
   the enum (see the ordering rule below), plus arms in `to_string()`,
   `density()`, `mass()`, `dimensions()`, optionally `visual_scale()`.
   ⚠️ Only `to_string()` and `mass()` are compiler-enforced; `density()`,
   `dimensions()` and `visual_scale()` have `_ =>` fallbacks. Forgetting
   `dimensions()` gives you a silent 0.5 m cube — **which also becomes the
   collider**, via `Body::collider()`.
2. The `.vox` under `assets/voxygen/voxel/object/`.
3. A `bone0`/`bone1` row in `assets/voxygen/voxel/object_manifest.ron`
   (unused bone → `central: ("armor.empty")`). A missing row logs an `error!`
   and falls back to `not_found` rather than crashing.
4. Spawn it server-side. **`Pos` + `Ori` + `Body` is not enough** — see the
   four traps below.

### The four traps in step 4

These are not theoretical; each is documented in this repo's own code, and
each produces a "it just doesn't work and nothing logs" session.

1. **No `Collider` ⇒ invisible.** `FigureMgr`'s join takes `&physics_states`
   non-optionally, and `PhysicsState` only exists for entities that have a
   `Collider`. `server/src/events/remote_sense.rs` spells it out in a comment:
   an absent `Collider` *"drops `PhysicsState` entirely, which silently drops
   the entity from the figure renderer"*. Use `Collider::Point` for a prop
   that must not push players around.
2. **`create_object` makes it fall.** `StateExt::create_object` attaches
   `Vel`, `Mass`, `Density` and a `CapsulePrism` collider sized from
   `dimensions()`. A "purely visual" prop is therefore a solid, gravity-driven
   body. Add `comp::Immovable` — `server/src/citadel.rs` does exactly this for
   the citadel turrets.
3. **No `Anchor` ⇒ deleted on chunk unload.** Any non-`Presence` entity with a
   `Pos` in an unloaded chunk is cleaned up by the server. `server/src/citadel.rs`
   adds `comp::Anchor::Chunk(home_chunk)` and comments the failure verbatim:
   entities created before a client can receive them are removed by the first
   unloaded-chunk cleanup. Required for anything persistent.
4. **`DeleteAfter` makes the model strobe for its last 10 seconds.**
   `FigureMgr::should_flicker` hides the entity when
   `time > spawned_at + timeout - 10.0 && (time * 8.0).fract() < 0.5`, in both
   the shadow and main passes. So **any effect with a timeout ≤ 10 s is
   invisible half the time, at 4 Hz, for its entire life** — which is most
   spell VFX. Either give it a ≥10 s timeout and delete it explicitly, or use
   one of the cheaper surfaces below instead of an entity.

`NapalmPool` (`server/src/state_ext.rs`, `create_pool`) is the working
template for a ground-attached, timed, glowing effect object; read it before
writing your own spawn.

⚠️ **`LightEmitter` is a globally capped resource, not a per-entity cost.**
`MAX_LIGHT_COUNT = 20` (`voxygen/src/scene/mod.rs`), and the scene sorts every
light by distance to the viewpoint and truncates. A handful of glowing VFX
props next to the player evict campfires, lanterns and each other. Use it
deliberately, not as decoration.

**Budget it like an entity, because it is one.** A `Body::Object` VFX prop is
a real, server-side, network-synced ECS entity: it joins the physics and
spatial-grid joins, it is replicated to every client in range, and
`Object::DeleteAfter` is polled each tick by `server/src/sys/object.rs`,
which emits a `DeleteEvent` once `now - spawned_at > timeout`. That is
several orders of magnitude more expensive per unit than a particle (56 bytes
in an instance buffer, CPU-free after spawn). So: **one prop per cast, not
one per visual element.** A rune circle is one entity; forty orbiting motes
are particles. If you find yourself spawning entities in a loop, you picked
the wrong surface.

### Two cheaper surfaces before you reach for an entity

- **`Outcome`** (`common/src/outcome.rs`) — no Uid, no region bookkeeping, no
  physics, no deletion event; distance-filtered per client in
  `server/src/sys/entity_sync.rs`. This is the right surface for **any
  one-shot visual**, and `PhantasmDissipated` is exactly that case.
- **The `CitadelPracticeSphere` timeline pattern** — `server/src/cmd.rs`
  spawns one entity carrying `Pos`/`Ori`/`DeleteAfter` **plus a single
  timeline component**, and the client evaluates the whole flight locally
  (`voxygen/src/scene/particle.rs`, `maintain_citadel_sphere_particles`). One
  synced component describes a whole moving effect instead of the server
  moving an entity every tick. This is Xindeler's own precedent for
  "spell VFX as an entity" and it is the one to copy.

### Enum ordering — the rule that is not what it looks like

`object::Body` and the NPC `Species` enums carry explicit `= N` discriminants,
which reads like a wire-stable tag. It is not: serde encodes enums by
**positional variant index**, and rtsim persists `comp::Body` through
`rmp_serde`, whose index-based externally-tagged encoding rtsim's own
regression tests call out as something that *"would have silently broken every
pre-existing save"*. So the real rule is **append at the end; never insert or
reorder**. The `= N` is documentation plus `FromPrimitive`, nothing more.

Animation available out of the box: `ObjectSkeleton` has exactly **2 bones**
and four animations — `idle`, `shoot`, `beam`, `turret`
(`voxygen/anim/src/object/`). `turret.rs` is Xindeler's, driven by
server-synced `CitadelTurretAngles`. Anything beyond "spin, pulse, or
two-part articulation" means writing a new `impl Animation` in
`voxygen/anim/src/object/`.

⚠️ One non-obvious detail if your prop is articulated: in `ObjectSkeleton`'s
`compute_matrices_inner`, `bone1` is deliberately **decorrelated from the
entity's `Ori`** for every object body *except* the two citadel cannons. A new
two-bone prop whose second bone should follow the entity's facing must be
added to that match arm, or it will ignore orientation.

**The hard constraint, stated once:** there is **no multi-frame `.vox`
animation anywhere in this engine**. You cannot author frame 1, frame 2,
frame 3 of an unfurling glyph. Motion is either bone transforms in Rust, or a
time-varying shader surface (`Fire` / `SwirlyCrystal`, reference 02), or
particles.

## Verdict per effect category

### ✅ Good `.vox` candidates — author these

| Effect | Why | Notes |
|---|---|---|
| **Ground AoE rune circles / sigils** | Flat, geometric, symmetric — exactly what parametric generation is for, and radius/spoke-count can come from the spell's own numbers | The engine *asks for this*: `maintain_char_state_particles`'s `GroundAoe` arm carries `TODO(magic-v1 polish): dedicated decal/ParticleMode; CultistFlame is a readable placeholder ring`. Also replaces the shockwave path's per-particle terrain raycasting (~2 µs each, measured peak 113/tick) |
| **Projectiles** (bolts, shards, orbs, spectral blades) | Zero new machinery: a projectile already *is* a `Body::Object` with a `.vox` | 43 such models already ship under `assets/voxygen/voxel/weapon/projectile/` |
| **Floating orbs, foci, wisps, motes** | Small, round, glowy — `Palette.add(col, GLOWY)` or `custom_indices` `SwirlyCrystal` gives live shimmer for one RON line | `Object::FloatingDisk` already exists as a hovering prop |
| **Solid conjurations** — ice shards/walls, stone spikes, thorn barriers, summoned weapons | Genuinely solid geometry that should cast shadows and read as an object | |
| **Totems, seals, wards, portals, braziers** | Persistent props; `portal.vox` and `gnarling_totem_*.vox` are the existing pattern | |
| **Spinning/pulsing arcane machinery** | Two-bone articulation is enough | The citadel cannon is exactly this |
| **`PhantasmDissipated`** | A Xindeler outcome (`common/src/outcome.rs`) that fires, carries `pos` *and* `body`, and **currently renders nothing** (an explicitly-named no-op arm in `ParticleMgr::handle_outcome` — so it is listed, just unimplemented) | A ready-made hook |

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
2. **A new `Body::Object` reusing an existing mesh at a different size.** The
   citadel's three laser bodies did exactly this — new body, new size/mass,
   existing `.vox`. `visual_scale()` (`common/src/comp/body/object.rs`)
   derives the render multiplier from the gameplay `dimensions()` ratio
   against a source body, so the art follows the size ladder instead of a
   second hand-tuned table. Copy that pattern rather than authoring a
   bigger model.
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
