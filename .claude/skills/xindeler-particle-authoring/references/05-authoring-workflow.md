# 05 — The authoring workflow

The recipe, the verify loop, and an end-to-end worked example that was actually
run against this tree.

## Step 0 — is this a new mode at all?

Work down the reuse checklist at the end of `references/04` first. In rough
order of effort:

1. **A different `specifier:` in an ability's RON file** — zero code
   (`references/03`).
2. **A new `FrontendSpecifier` variant mapped to an existing `ParticleMode`** —
   one `common/` enum variant + one match arm in `particle.rs`.
3. **A new emission site reusing existing modes** — e.g. a new `Outcome` arm, or
   a new `BuffKind` arm.
4. **A recolour**: copy an existing `case`, change the `vec4`. Six shipped
   fireworks are exactly this.
5. **A genuinely new `case`** — everything below.

## Step 1 — claim a number

```bash
python3 tools/particles/particle_modes.py
```

prints `Next free mode number`. **Append the variant at the end of
`ParticleMode`** (`voxygen/src/render/pipelines/particle.rs:62`) with that
number; never insert or renumber. Give it a doc comment saying what spell or
system owns it, as the two `Citadel*` variants do.

## Step 2 — write the shader case

Add `const int YOUR_MODE = N;` to the constant block at the top of
`assets/voxygen/shaders/particle-vert.glsl` (keep it in numeric order, next to
the others) and a `case YOUR_MODE:` **above `default:`**.

Fill an `Attr` using the library in `references/02`. Watch the switch-scope
trap: cases share one scope, so give your locals distinct names.

## Step 3 — write the emission site

In `voxygen/src/scene/particle.rs`, at the entry point from `references/03`'s
decision table. Non-negotiables at the emission site:

- honour `scene_data.particles_chance` (via `add_particles` or `push_particle`);
- drive continuous emission through `self.scheduler.heartbeats(...)`, never
  "n per frame";
- derive the count from the spell's own `power` / `radius` / `duration` where
  the trigger has them;
- pass `.with_light(...)` if the effect can occur indoors;
- pick the right constructor — `new`, `new_directed`, or `new_colored` — and
  make sure the shader case agrees about what `inst_dir` means.

## Step 4 — verify

```bash
# 1. enum ↔ shader agreement, and the authoring summary       (< 1 s, no build)
python3 tools/particles/particle_modes.py

# 2. the same invariant, deeper, plus two real GLSL compiles  (needs voxygen built)
VELOREN_ASSETS="$(pwd)/assets" cargo test -p xindeler-voxygen \
  --no-default-features --features shaderc-from-source particle

# 3. the crate still builds with the feature set CI uses
cargo clippy -p xindeler-voxygen --locked --no-default-features \
  --features="default-publish" -- -D warnings
cargo fmt --all -- --check
```

Three tests run under (2):

- `shader_modes_match_particle_modes` — enumerates the real enum (not a regex
  over it), requires every mode to have a `const int` *and* a `case` *and* a
  `break;`, rejects stale shader constants, and cross-checks the one mode number
  `particle-frag.glsl` redeclares for itself.
- `particle_shaders_compile` — `shaderc`, the renderer's **fallback** compiler.
  Stricter, and it reports the **exact GLSL line number** of a syntax error.
- `particle_shaders_parse_with_naga` — naga, the renderer's **default**
  compiler (`PipelineModes::enable_naga` is on unless
  `VELOREN_DISABLE_NAGA_SHADERS` is set). The two accept slightly different
  GLSL dialects, so a shader that only passes one can still fail for a player.

Together these are the only offline GLSL validation available: there is no
`glslc`, `glslangValidator` or `naga` binary on this machine. Note **CI runs
clippy and `fmt`, not `cargo test`** — clippy `--all-targets` compiles these
tests so they cannot rot, and `ci-code-quality.yml` runs the Python checker
directly, but the two shader-compile tests only ever run when a human or an
agent runs them. Run them.

## Step 5 — look at it

Nothing above can tell you whether the effect *reads*. Run the client:

```bash
# macOS (per the repo CLAUDE.md)
cargo run --bin xindeler-voxygen --no-default-features \
  --features default-publish,shaderc-from-source,egui-ui
```

Then, in game:

- `/outcome <Variant> …` fires any `Outcome` on demand (`references/03`) — the
  fastest trigger for a one-shot effect.
- **Edit `particle-vert.glsl` while the client runs.** The asset watcher
  rebuilds the pipelines on save (`voxygen/src/render/renderer/mod.rs:1323`);
  a broken edit logs `"Could not recreate shaders from assets due to an error"`
  (`mod.rs:1297`) and keeps the old pipeline instead of crashing. Tuning
  colours, sizes and easing constants this way takes seconds per iteration and
  is the real authoring loop. Changing the *Rust* side still needs a rebuild.
- Check it at **Low graphics** (`particles_chance = 0.1`) as well as Ultra. An
  effect that is unreadable at 0.1 needs fewer, bigger particles.
- Check it **at night and in a cave** if it is not fully emissive, and **in
  rain/wind** if it uses `blown_by_wind`.

If you are an agent and cannot run a GUI client, say so plainly and hand back a
smoke-test checklist (build command, how to trigger it, what "wrong" looks
like) rather than implying visual sign-off.

## Worked example, verified against this tree

This exact sequence was run on this branch and then reverted; it is the shape
every new effect follows.

**1. Claim the number.** The checker reported `Next free mode number : 86`.

**2. Add the variant** in `voxygen/src/render/pipelines/particle.rs`:

```rust
    CitadelSphere = 85,
    ToxicMiasma = 86,
```

**3. Run the checker before touching the shader** — it fails, which is the
point:

```
- ToxicMiasma (86) has no `const int … = 86;` in particle-vert.glsl;
  it renders as the shader's `default:` white motes
```

**4. Add the shader side** — a constant next to `CITADEL_SPHERE`, and a case
above `default:`:

```glsl
const int TOXIC_MIASMA = 86;

        case TOXIC_MIASMA:
            f_reflect = 0.0;
            float miasma_col = 1.4 + rand5 * 0.4;
            attr = Attr(
                (inst_dir * slow_end(0.4))
                    + vec3(sin(lifetime() * 0.7 + rand0 * 6.0),
                           sin(lifetime() * 0.6 + rand1 * 6.0),
                           0.35 * lifetime()) * 0.4
                    + blown_by_wind(3.0, 0.2),
                vec3(7.0 * (1.0 - slow_start(0.5)) * slow_end(0.3)),
                vec4(vec3(0.22, 0.62, 0.24) * miasma_col, start_end(1.0, 0.05)),
                spin_in_axis(vec3(rand6, rand7, rand8), rand9 * 3.0 + lifetime() * 0.6)
            );
            break;
```

Every piece is borrowed: the directed spread from `FLAMETHROWER`, the
both-ends-eased size from `BUBBLES`, the heavy wind mass from `BLACK_SMOKE`, the
lazy sine wander from `FIREFLY`, the green palette from `POISON`. The only
original decisions are the numbers — which is the normal ratio.

**5. Verify.** Checker green; both shaders compiled to SPIR-V in 0.05 s. A
deliberately introduced missing semicolon produced:

```
particle-vert.glsl failed to compile:
particle-vert.glsl:1382: error: '' :  syntax error, unexpected IDENTIFIER,
                                      expecting COMMA or SEMICOLON
```

which is the whole value of the compile test — that line number would otherwise
have cost a full client launch to find.

**6. What was *not* verified**, and could not be in a headless session: whether
a slow drifting green cloud actually reads as miasma rather than as fog, at
what density, and against which backgrounds. That is a human judgement and
belongs in a smoke-test checklist, not in a test.

## Performance budget

- A particle costs no CPU after spawn, but it is **72 B resident / 56 B
  uploaded**, and each frame the whole pool is re-`collect`ed into two fresh
  `Vec`s and two freshly-allocated GPU buffers, and scanned twice. The full
  model, with the numbers, is at the end of `references/01` — read it before
  sizing anything above a few thousand particles.
- There is **no global cap**. Real shipped rates for calibration: a `Cultist`
  beam emits **960 particles/second** (`particle.rs:2788`); one
  `Outcome::Lightning` spawns **800 at once** (`particle.rs:95`); a red
  explosion spawns `75 × power` (`particle.rs:154`). The only rate in the tree
  with no code-side ceiling is the fire-pillar one, set in ability RON
  (`references/03`).
- `count × lifespan` is the number that matters, not the spawn burst.
- Terrain raycasts inside an emitter are the expensive thing — the code's own
  note says ~2 µs each with a peak of 113/tick (`particle.rs:4171`), but that is
  an **inherited upstream comment on unknown hardware, not a measurement in this
  fork**, and it hedges itself. Treat it as an order of magnitude, and only pay
  for raycasts when the effect must follow uneven ground.
- `Light::new` from an emitter does not merely "compete" for the global 20
  (`voxygen/src/scene/mod.rs:71`): the scene sorts all lights by distance to the
  viewpoint and truncates (`voxygen/src/scene/mod.rs:976`), so a light near the
  camera **evicts** the 20th-nearest world light — nearby torches and campfires
  visibly pop off for as long as your effect lives. Gate it on
  `scene_data.flashing_lights_enabled`, as the beam code does.

## Shipping

- Branch off the **current working branch** (`git branch --show-current` —
  do not assume `development`), one PR, never merge.
- Run the specialist reviewers (`ecs-design-reviewer`,
  `game-architecture-reviewer`, `rust-perf-reviewer`) against your own diff
  before opening the PR, not after.
- No AI attribution in commits.
- Shaders and `particle.rs` are ordinary text files — **not** Git LFS. Only the
  binary `.vox` assets are (see `xindeler-voxel-authoring`).
- Design docs go to `docs/design/` on its own branch + PR, with a `git pull`
  before every read and write there.

## The known gap in the enum

`ParticleMode::SnowStorm = 44` has no shader constant, no `case`, and no
emitter anywhere in the tree — it is inherited dead weight, and it is the reason
the invariant test carries an explicit exemption list
(`MODES_WITHOUT_SHADER_CASE`). This was investigated again (2026-09) and left
as-is on purpose: this engine's weather system (`common/src/weather.rs`) has no
snow/blizzard concept at all to hook a snowstorm burst to, and deleting the
enum variant would only widen the diff against any future upstream merge for
no gameplay gain. If a real snow-storm weather event is ever added, wire
`SnowStorm` then; until then it stays declared-and-exempted, not deleted.

`StaticSmoke = 24` **is no longer a gap** — it now has a real emitter:
`BuffKind::Burning` spawns a few low-rate smoulder wisps around the body
alongside its flame licks (`maintain_buff_particles`, `particle.rs`; `Cursed`,
a magical flame rather than real fire, does not get it). Do not add to either
category going forward: a mode you introduce must be declared, handled, and
emitted, or it should not exist yet.
