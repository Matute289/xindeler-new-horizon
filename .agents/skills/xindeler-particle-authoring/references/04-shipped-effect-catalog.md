# 04 — The 86 declared modes, by phenomenon

**Start here.** Most spell-VFX asks are already 80 % built. This catalogue is
grouped by what the effect *is*, not by the order the enum happens to be in, so
you can find the nearest existing thing and either reuse it outright, reuse it
with a different `specifier:` in RON, or copy its `case` as the starting point
for a new one.

Numbers are the `ParticleMode` discriminant
(`voxygen/src/render/pipelines/particle.rs:62`); line numbers are `case` sites
in `assets/voxygen/shaders/particle-vert.glsl`.

`python3 tools/particles/particle_modes.py` prints the live counts, the next
free number, and any mode that is declared but never emitted.

## Fire and heat

| Mode | # | Line | What it is / the idiom to steal |
|---|---|---|---|
| `CampfireFire` | 1 | 334 | The reference flame: rises, wobbles on two sines, cools yellow→red via `3 + rand5*0.3 - 0.8*percent()` |
| `FireBowl` | 18 | 346 | Brazier flame: fixed small size, ring-biased spawn offset |
| `FlameThrower` | 16 | 544 | **The cone-burst archetype.** `inst_dir * slow_end(1.5)` + jitter + wind. Most new fire effects should start here |
| `FlamethrowerBlue` | 79 | 1293 | The same case recoloured — the cheapest way to make a variant |
| `Explosion` | 20 | 553 | `inst_dir` scaled by a per-particle random, `slow_end(0.25)`, plus 0.3 gravity; colour at `start_end(3.0, 0.0)` for a bright flash |
| `FireShockwave` | 17 | 572 | Vertical fire column for a ring; height = `lifetime() * 10` |
| `FireLowShockwave` | 73 | 581 | Ground-hugging variant; ballistic, shrinks linearly |
| `CultistFlame` | 23 | 593 | Purple flame. Currently doubles as the generic ground-AoE ring placeholder (`particle.rs:2362`) |
| `FieryBurst`/`Vortex`/`Sparks`/`Ash` | 48–51 | 827–891 | A **four-mode composite**: one visual built from four emitters with different motion. The pattern to copy for a "big" spell |
| `FieryTornado`, `FlameTornado` | 52, 62 | 935, 1115 | `spiral_motion` with a fire palette |
| `PhoenixCloud`, `FieryDropletTrace`, `EnergyPhoenix`, `PhoenixBeam`, `PhoenixBuildUpAim` | 53, 54, 55, 56, 57 | 949–1031 | A full boss-ability VFX set: build-up, beam, cloud, trace. The most elaborate worked example in the file |
| `FireGigasAsh`/`Whirlwind`/`Overheat`/`Explosion` | 67–70 | 1177–1206 | A second full boss set |
| `FirePillarIndicator`, `FirePillar` | 71, 72 | 1215, 1224 | **Telegraph + payoff pair** — the right shape for any "danger zone then damage" spell |
| `FlameCloakOrbit` | 80 | 1302 | Flames orbiting the caster via `spiral_motion`, with both ends eased |

## Ice, water and cold

| Mode | # | Line | Notes |
|---|---|---|---|
| `Ice` | 21 | 562 | Directed shard burst; `f_reflect = 0` with the comment *"Ice doesn't reflect to look like magic"* |
| `IceSpikes` | 31 | 664 | A **static spike**: zero offset, scale grows then shrinks on `0.5 - abs(0.5 - slow_end(0.5))`, height from `length(inst_dir)`. The idiom for a thing that erupts and retracts in place |
| `IceWhirlwind` | 47 | 818 | Spiral with a *negative* scale (mirrors the cube) and an expanding radius |
| `Water`, `Bubbles`, `WaterFoam`, `Bubble`, `BubbleAmbient` | 30, 29, 64, 76, 83 | 653, 642, 1134, 1252, 1264 | `WaterFoam` is the only mode that writes a different deferred material (`MAT_PUDDLE`, `particle-frag.glsl:120`) |
| `Snow`, `GigaSnow`, `SnowStorm` | 19, 42, 44 | 467, 771, — | `Snow` samples `alt_at()` to fall from the sky onto terrain. **`SnowStorm` has no shader case and is emitted by nothing** — see `references/05` |
| `Steam` | 39 | 738 | Mint-green magic steam; both ends eased |
| `Drip` | 32 | 674 | Ballistic yellow droplet |

## Lightning and electricity

| Mode | # | Line | Notes |
|---|---|---|---|
| `Lightning` | 38 | 726 | The strike itself: travels `inst_dir * percent()` with a `fract`-based zig-zag offset, scale growing with distance from the camera so a 600 m bolt stays visible, colour `vec4(10, 10, 25, 1)`, and **`identity()` rotation on purpose** |
| `ElectricSparks` | 78 | 1282 | Short arcs: `align_to_axis(inst_dir)` with `scale = vec3(len, 0.15, 0.15)`. The idiom for any thin straight streak |
| `CyclopsCharge` | 43 | 780 | Charge-up gather |

A lightning strike is *also* a full-screen event: `Outcome::Lightning` sets
`Scene::last_lightning` (`voxygen/src/scene/mod.rs:555`), which feeds the sky
flash (`include/sky.glsl:212`) and a giant transient point light
(`include/point_glow.glsl:91`). If a new spell should flash the world, that is
the mechanism — not more particles.

## Poison, gas, acid, ink

| Mode | # | Line | Notes |
|---|---|---|---|
| `Poison` | 63 | 1124 | `FLAMETHROWER`'s motion with a green palette — a *directed burst*, not a lingering cloud |
| `Ink` | 46 | 807 | Same shape, near-black, with a random darkening step |
| `Spore` | 60 | 1090 | Drifting spores |
| `PotionSickness` | 41 | 759 | Nausea motes; emitted only during a 0.5 s window after drinking (`particle.rs:3485`) |

**There is no slow, lingering, volumetric gas cloud today.** Every green/toxic
mode in the tree is a fast directed burst. A real "poison cloud that hangs in
the air" is genuinely new work, and it is a *long lifespan + heavy
`blown_by_wind` mass + both-ends-eased scale* problem, not a new motion
function. `shockwave::FrontendSpecifier::AcidCloud`
(`common/src/comp/shockwave.rs:62`) already exists as a data-side name to hang
it on.

## Smoke, dust, ash

| Mode | # | Line | Notes |
|---|---|---|---|
| `CampfireSmoke` | 0 | 312 | The reference smoke: slow rise, `blown_by_wind(1.0, 0.25)`, tumbling, fades by shrinking |
| `BlackSmoke` | 37 | 323 | Heavier: `blown_by_wind(7.0, 0.5)` — higher `mass` delays the drift |
| `StaticSmoke` | 24 | 603 | Non-rising smoke. **Declared but emitted by nothing** |
| `PipeSmoke`, `TrainSmoke` | 74, 75 | 1233, 1244 | Small ambient sources |
| `Dust`, `CaveDust` | 81, 82 | 1317, 1330 | `Dust` reads `inst_dir` **as a colour** through `srgb_to_linear` — the trap from `references/01` in the wild |
| `Airflow` | 59 | 1073 | Wind visualisation |

## Impact, debris, gore

| Mode | # | Line | Notes |
|---|---|---|---|
| `Shrapnel`, `BigShrapnel`, `ClayShrapnel` | 3, 27, 58 | 369, 382, 1061 | Ballistic debris; `Shrapnel` uses `on_floor()` for a damped bounce |
| `GunPowderSpark` | 2 | 358 | Bright ballistic spark |
| `Blood` | 25 | 611 | Pure red, ballistic, no emission |
| `GroundShockwave` | 13 | 504 | A **brown terrain column** whose height oscillates on a sine. Spawned by a per-particle terrain raycast (`particle.rs:4176`) so it sits on real ground |

## Energy, buffs, auras, beams

| Mode | # | Line | Notes |
|---|---|---|---|
| `EnergyHealing`, `EnergyNature`, `EnergyBuffing` | 14, 15, 35 | 512, 534, 706 | **One motion, three colours** — identical `spiral_motion` bodies. The template for a new school-coloured aura |
| `LifestealBeam` | 22 | 522 | The most animated case in the file: three independent `tick_loop` sines drive the colour, so the beam pulses in wall-clock phase |
| `Laser`, `WebStrand` | 28, 36 | 632, 716 | The stretch-along-`inst_dir` stroke; `scale = vec3(1,1,50)` |
| `CitadelLaser` | 84 | 1344 | Xindeler's own: `LASER`'s geometry, cyan/violet palette |
| `CitadelSphere` | 85 | 1358 | Xindeler's own, and the **only mode drawn with a real `.vox` model** (`particle.rs:4999`). Fixed offset/scale on purpose — the CPU respawns it at the true position every ~250 ms (`particle.rs:854`) |
| `Enraged` | 26 | 622 | Red rage motes |
| `Death` | 34 | 694 | White ascending wisps |
| `Transformation` | 66 | 1156 | Polymorph flash |
| `PortalFizz` | 45 | 790 | Colours itself by the **angle between the viewer and the particle's direction** (`particle-vert.glsl:801`) — the only view-dependent mode |

## Ambient world life

`Leaf` (10), `Firefly` (11), `Bee` (12), `Tornado` (33), `BarrelOrgan` (40),
`SurpriseEgg` (61), `EngineJet` (65), `ElephantVacuum` (77), and the six
`Firework*` modes (4–9). `Firefly` and `Bee` are the model for "a small thing
wandering on layered sines"; the fireworks are six copies of one case differing
only in the `mix()` colour pair — the clearest demonstration in the file that a
**recolour is a copy-paste, not a design problem**.

## Reuse checklist

Before adding mode 86, ask in order:

1. Does an existing mode already look right? (`/outcome` can show you several
   of them in seconds.)
2. Does the mechanic take a `FrontendSpecifier` that maps to an existing mode?
   → one RON line, zero code (`references/03`).
3. Is this a recolour of an existing case? → copy the case, change the `vec4`,
   done. Six fireworks and `FlamethrowerBlue` are precedent.
4. Is this a *composite* — several looks at once? → several emitters on one
   trigger, like `FieryBurst`'s four modes. Do **not** try to express two
   different behaviours in one `case`.
5. Only then write genuinely new motion.
