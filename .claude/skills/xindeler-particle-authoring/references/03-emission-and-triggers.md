# 03 — Emission: the six entry points and how a spell reaches them

Writing the shader is the fun half. **Choosing the entry point is the half that
decides whether the effect is correct**, because each one has a different
lifetime, a different authority, and a different cost.

## The decision table

| The effect should… | Entry point | Function |
|---|---|---|
| fire once, at a point, when something happens | **`Outcome`** | `ParticleMgr::handle_outcome` (`particle.rs:83`) |
| play while a caster is in a particular state / stage | **`CharacterState`** | `maintain_char_state_particles` (`particle.rs:1760`) |
| stick to a target for as long as a status lasts | **`Buffs`** | `maintain_buff_particles` (`particle.rs:3423`) |
| fill an area for as long as an aura lasts | **`Auras`** | `maintain_aura_particles` (`particle.rs:3151`) |
| ride an expanding ground ring | **`Shockwave`** | `maintain_shockwave_particles` (`particle.rs:4105`) |
| stream continuously down a cone or ray | **`Beam`** | `maintain_beam_particles` (`particle.rs:2764`) |
| emit forever from a body, block or fluid | **body / block / fluid** | `maintain_body_particles` (1082), `maintain_block_particles` (3686), `maintain_fluid_particles` (1036) |

Everything runs from `ParticleMgr::maintain` (`particle.rs:749`), once per
frame, client-side only. **The server never spawns a particle.** It spawns the
*state* — an outcome, a component — and every client that can see it decides
what to draw.

## 1. `Outcome` — the right default for one-shot spell VFX

`common/src/outcome.rs:29` defines ~50 variants; its own doc comment says an
outcome *"represents the final result of an instantaneous event… something for
frontends to listen to"*. It carries no entity, no Uid bookkeeping, no physics
and no deletion event, and the server distance-filters it per client by the
player's entity view distance (`server/src/sys/entity_sync.rs:487`).

Emitting one from game code is a single line:

```rust
outcome_emitter.emit(Outcome::Explosion { pos, power, radius, is_attack, reagent });
```

(`server/src/events/entity_manipulation.rs:3484`; `common/src/event.rs:52`
carries it as `CreateOutcome` from `common/` states such as
`common/src/states/transform.rs:89`.)

Handling it is a match arm in `ParticleMgr::handle_outcome`. The worked example
to copy is `Outcome::Explosion` (`particle.rs:123`): it branches on
`Reagent`, and each branch calls `add_particles` with a count derived from the
spell's own numbers — `(75.0 * power.abs()) as usize` for a red blast
(`particle.rs:154`), `(4.0 * radius.powi(2)) as usize` for a gigas blast
(`particle.rs:230`). **Deriving the count from the spell's power/radius rather
than a constant is the house style**; copy it.

> Adding a *new* `Outcome` variant is a wire-format change (`Serialize` +
> `ServerGeneral::Outcomes`). Append it at the end, and check whether an
> existing variant already carries what you need before adding one.

## 2. `CharacterState` — VFX that plays during a cast

`maintain_char_state_particles` (`particle.rs:1760`) is ~1,000 lines matching on
the caster's current `CharacterState` and, usually, its `StageSection`
(`Buildup` / `Action` / `Recover`). This is where "the staff glows while
charging, then bursts on release" lives.

Many of these states carry their own `FrontendSpecifier` enum — see
`common/src/states/{basic_melee,dash_melee,self_buff,blink,transform,…}.rs`
— which is set **in the ability's RON file**. That is the data surface: see
§ "Shipping a new look as data" below.

The house pattern for a caster-attached effect, from `particle.rs:1835`:

```rust
Particle::new(…)
    .with_light(char_state.meta.last_light, char_state.meta.last_glow.1)
```

Use it. Without `with_light`, the particle assumes full sunlight and glows
wrongly in a dungeon (`references/01`).

There is a standing gap here worth knowing about: the generic
`CharacterState::GroundAoe` arm renders its ring as `CultistFlame` with the
comment *"TODO(magic-v1 polish): dedicated decal/ParticleMode; CultistFlame is
a readable placeholder ring"* (`particle.rs:2362`).

## 3. `Buffs` / `Auras` — status-effect and area VFX

`maintain_buff_particles` (`particle.rs:3423`) matches on `buff::BuffKind` —
`Cursed` and `Burning` share a flame emitter, `PotionSickness` only plays for
the 1.0–1.5 s window after the drink animation (`particle.rs:3485`),
`Frenzied` gets `Enraged`. **A new status effect gets its VFX here, by
`BuffKind`, with no new outcome and no shader work if an existing mode fits.**

`maintain_aura_particles` (`particle.rs:3151`) matches on `aura::AuraKind` and
scales the count by area: `aura.radius.powi(2) as usize * heartbeats / 300`
(`particle.rs:3177`). It also uses the nice `rand_dist = radius * (1.0 -
rng.random::<f32>().powi(100))` trick to bias particles toward the aura's rim.

## 4. `Shockwave` — expanding ground rings

`maintain_shockwave_particles` (`particle.rs:4105`) reconstructs the ring's
current radius from `creation` time and `speed`, then distributes particles
along the arc. It switches on `shockwave::FrontendSpecifier`
(`common/src/comp/shockwave.rs:53`): `Ground`, `Fire`, `FireLow`, `Water`,
`Ice`, `IceSpikes`, `Steam`, `Poison`, `AcidCloud`, `Ink`, `Lightning`.

Two things to copy or avoid:

- **Sub-tick interpolation.** `Ground` runs a 2 ms heartbeat and back-dates each
  batch by `scaled_speed * 1000.0 * heartbeat` (`particle.rs:4151`) so a fast
  ring does not look like discrete puffs at low frame rates.
- **Terrain raycasting is expensive.** `Ground` and `FireLow` cast a 20 m
  vertical ray per particle to sit the effect on the actual ground; the code
  carries a measured note — *"each ray is ~2 µs; at 30 FPS it peaked at 113
  rays in a tick"* (`particle.rs:4171`). Only pay this when the effect must
  hug uneven terrain.

## 5. `Beam` — sustained cones and rays

`maintain_beam_particles` (`particle.rs:2764`) switches on
`beam::FrontendSpecifier` (`common/src/comp/beam.rs:37`) and converts a
per-specifier **particles-per-second** rate into a per-tick count
(`particle.rs:2775`):

| Specifier | particles/s |
|---|---|
| `Cultist` | 960 |
| `FireGigasOverheat` | 1600 |
| `LifestealBeam` | 420 |
| `Flamethrower`, `Bubbles`, `Steam`, `Frost`, `Poison`, `Ink`, `PhoenixLaser`, `Gravewarden` | 300 |
| `WebStrand` | 180 |
| `Lightning` | 120 |
| `FirePillar`, `FlameWallPillar` | `40 × end_radius²` — **the one uncapped row**: `end_radius` comes from ability RON (e.g. `assets/common/abilities/custom/gigas_fire/fire_pillars.ron`), so any radius above ~6.3 exceeds `FireGigasOverheat`'s 1 600/s with no code review |

Three details matter:

- **The tick budget is clamped**: `heartbeats(1 ms).min(100)`
  (`particle.rs:2771`), with the comment *"so at less than 10 FPS particle
  generation work doesn't increase frame cost further"*.
- **Beams stop at walls** via `Particle::new_directed_with_collision`
  (`particle.rs:5230`), which shortens both the end position and the lifespan by
  the raycast ratio. Any new beam should use it.
- **Beams may push a point light**, but only behind
  `scene_data.flashing_lights_enabled` (`particle.rs:2817`) — an accessibility
  setting, not a quality one — and lights are globally capped at
  `MAX_LIGHT_COUNT = 20` (`voxygen/src/scene/mod.rs:71`).

## 6. Ambient — bodies, blocks, fluids

`maintain_body_particles` (1082) for creature auras, `maintain_block_particles`
(3686) for the world (campfires, fireflies, leaves, bees, dust — driven by
`BlocksOfInterest`), `maintain_fluid_particles` (1036) for water. Xindeler adds
`maintain_pool_particles` (4890, `comp::Pool` — a burning ground pool),
`maintain_arcing_particles` (4790, `comp::Arcing` — chained lightning arcs) and
the two citadel timeline maintainers (805, 854). Spell VFX rarely belongs here,
but `maintain_pool_particles` is the model for a **persistent ground effect
driven by one server component**, which several spell asks turn out to be.

## Rate control: `HeartbeatScheduler`, `add_particles`, `particles_chance`

```rust
// N times per second, frame-rate independent:
let amount = usize::from(self.scheduler.heartbeats(Duration::from_millis(15)));
self.add_particles(scene_data.particles_chance, amount, || Particle::new(…));

// A single particle, probabilistically kept:
self.push_particle(scene_data.particles_chance, Particle::new(…));
```

- **`HeartbeatScheduler`** (`particle.rs:5037`) converts a wall-clock interval
  into "how many times did this elapse since the last frame", carrying the
  fractional remainder forward (`particle.rs:5074`). Always drive continuous
  emission through it; never emit "n per frame", which makes the effect
  frame-rate dependent.
- **`add_particles(chance, amount, f)`** (`particle.rs:900`) calls `f` exactly
  `(amount as f32 * chance) as usize` times — it *scales* the count.
- **`push_particle(chance, p)`** (`particle.rs:893`) rolls a random bool — it
  *drops* the particle. Use it inside a loop that already computed positions.
- **`scene_data.particles_chance`** is the graphics preset: **0.1 on Low**,
  0.25, 0.5, 0.75, 1.0 (`voxygen/src/settings/graphics.rs:91`–`215`). Honour it
  in any emitter whose particle count is a *density*, or Low-spec players get
  the full cost. An effect whose readability collapses at 0.1 (a beam that
  becomes three dots) is a design bug — prefer fewer, larger particles over many
  small ones.
  **The named exception** is an emitter that draws exactly one deterministic
  visual per entity per frame: the two citadel maintainers push straight onto
  the pool (`particle.rs:834`, `particle.rs:880`) because probabilistically
  dropping their single stroke would make the beam strobe. If you are writing
  one particle per entity per frame, bypassing the roll is correct; if you are
  writing a cloud, it is not. (`maintain_block_particles`'s campfire-smoke path
  at `particle.rs:4088` also bypasses it — that one is simply inconsistent with
  the rest of its own function, not a pattern to copy.)
- **`scene_data.particles_enabled`** short-circuits `maintain` entirely
  (`particle.rs:758`) and clears the buffers. Never assume your maintainer ran.

## Shipping a new look as data (no Rust, no GLSL)

Several mechanics select their VFX from the ability's own RON file. A
`shockwave` ability carries `specifier: Ground`
(`assets/common/abilities/hammer/tremor.ron:16`), a beam carries
`specifier: LifestealBeam`
(`assets/common/abilities/vampire/bloodmoon_bat/lifestealbeam.ron:14`), an
aura carries `specifier: Some(HealingAura)`, a blink carries
`frontend_specifier: Some(CultistFlame)`.

**So the cheapest new spell VFX is a different `specifier:` on an existing
mechanic.** Check that first: eleven shockwave looks and fifteen beam looks
already ship. Adding a *new* specifier variant costs one enum variant in
`common/` plus one match arm in `particle.rs` — still no shader work if you
reuse an existing `ParticleMode`, and it is the right layer for "this school of
magic is green" style asks (see the `xindeler-abilities` and
`game-architecture` skills for where ability data belongs).

## Triggering it for testing

`/outcome <Variant> [args…]` is an admin command
(`common/src/cmd.rs:1054`, handler at `server/src/cmd.rs:4722`) that fires any
`Outcome` variant at a position on demand — the fastest way to see a new
one-shot effect without building a spell around it:

```
/outcome Explosion 1.0 5.0 true Red
/outcome Lightning
/outcome PhantasmDissipated
```

Positions default to the target's position; `power`, `radius`, `is_attack` and
`reagent` parse in order. Effects driven by a `CharacterState`, `Buff` or
`Beam` need the actual ability — use `/give_item` plus the ability, or spawn the
NPC that has it.
