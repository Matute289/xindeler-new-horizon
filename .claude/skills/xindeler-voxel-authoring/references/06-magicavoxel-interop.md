# 06 — MagicaVoxel interop: the scene graph, and the "animation script" verdict

Reference 03 establishes that this engine has **no keyframe voxel animation**:
motion is bone matrices applied to rigid, separately-modelled segments. That
raised an obvious question — *is there prior art worth importing for baked
multi-frame voxel animation, and is there a better way to author bone pivots
than tuning manifest offsets by hand?*

Matías supplied two references for exactly that. This file is the verdict on
both, plus the one genuinely useful mechanism that came out of the
investigation.

## The short version

| Thing | What it actually is | Verdict |
|---|---|---|
| `Matute289/MagicaVoxel-Animation-Script` | A GUI-automation robot that types **camera and lighting** commands into MagicaVoxel's console and screenshots the result | **Does not apply.** It animates a renderer, not a model. Nothing it produces is loadable. |
| MagicaVoxel's own frame animation (`a_time`) | Per-frame *whole-model snapshots*, no bones, no tweening | Data lands in `models[]` and is readable, but the engine can only ever draw **one** of them. |
| The MagicaVoxel **scene graph** (`nTRN`/`nGRP`/`nSHP`) | Named parts placed at translations in an assembled figure | **The real find.** It is the only record of which model is which part, and `voxlib.read_scene()` now reads it. |

## `Matute289/MagicaVoxel-Animation-Script` — what it actually does

Read, not guessed. It is an unmodified fork of
[`DimasVoxel/MagicaVoxel-Animation-Script`](https://github.com/DimasVoxel/MagicaVoxel-Animation-Script)
(fork created 2026-09-08; `git log upstream/main..HEAD` is **empty** — zero
commits of its own, so there is nothing Xindeler-specific in it to preserve).
Upstream's last commit is 2022-05-21 and it targets MagicaVoxel 0.99.7.

The mechanism, from `Animation Script/Animation-Script.py`:

1. A `dearpygui` "Config Generator" writes a `config.json` of keyframes. Each
   keyframe holds a dict of **MagicaVoxel console parameters** and options
   (frame count, seconds per render, interpolation, rotation direction).
2. The runner lerps (or de Casteljau-béziers) each parameter across N frames.
3. For each frame it builds a console command string like
   `cam ry 45.0 | cam rx 30.0 | set pt_expo 1.2 | snap scene | ` …
4. It then **types that string into MagicaVoxel via simulated keystrokes**:
   `pydi.press('f1')` (F2 on macOS) to open the in-app console, `pyperclip`
   to put the command on the clipboard, Ctrl/Cmd-V to paste, Enter to run,
   Enter again to dismiss the save-render dialog. It polls the OS for the
   foreground window title and pauses if MagicaVoxel loses focus, so it does
   not spray keystrokes into your Discord.
5. `snap scene` makes MagicaVoxel render and save a **PNG**. The README:
   *"The renders are saved in the magicavoxel export folder."*

The complete list of animatable parameters (`Config Generator 2.py`) is
`cam rx/ry/rz`, `cam x/y/z`, `cam zoom`, `pt_fov`, `pt_dof`, `pt_focus`,
`pt_blade_rot`, `pt_expo`, the four `pt_bloom_*`, and the lighting set
`pt_sun_p/y/area`, `pt_isun`, `pt_isky`, `pt_ray_d`, `pt_mie_d/g`, `pt_o3_d`,
`pt_ibl_i/rot`, `pt_fog_et/eg`.

**Every one of those is a camera or path-tracer parameter.** Not one touches
geometry, a model, a part, a bone or a voxel.

### Why it cannot apply here

- **Its output is a folder of PNGs**, i.e. a finished video. The engine
  consumes `.vox` voxel data. There is no conversion, because no geometry
  information is produced at any point.
- **It animates the offline path tracer**, which this engine does not use and
  could not use — figures are meshed into a shared atlas and drawn by
  `figure-vert.glsl`.
- **It requires a human-interactive MagicaVoxel 0.99.7** in the foreground,
  driven by synthetic keystrokes. That is not automatable in CI, on the VPS,
  or in an agent session.

It is a good tool for what it is: making a **promotional turntable or
flythrough render** of a model — a key-art/marketing asset, the kind of thing
`xindeler-graphic-assets` covers. Treat it as a marketing-render tool, filed
next to the `meshy`/`voxelai` external-tool skills, and **not** as part of the
game-asset pipeline. Nothing about it belongs in `tools/voxel/`.

### The one thing it does touch in the model, and why that is also a dead end

`animationHandler()` emits `set a_time <n>`, stepping MagicaVoxel 0.99.7's own
frame-animation timeline so the model animation plays while the camera moves.
That is the closest the script gets to model animation — and it is a
*playback* command to the editor, not an export.

MagicaVoxel's frame animation is itself **frame-by-frame whole-model
snapshots**: you duplicate the model and re-sculpt it per frame. No bones, no
rigging, no tweening. So even conceptually it is the opposite of what this
engine does — it trades a 15-part rigid skeleton for N complete copies of the
whole creature.

## Could baked frames ever be consumed here? (No, and here is the precise gap)

This is worth spelling out because the answer is *nearly* yes, which is
exactly the sort of thing that gets assumed.

**What works:** MagicaVoxel stores animation frames as consecutive
`SIZE`/`XYZI` models in `MAIN` — the same flat `models[]` list that
`Segment::from_vox_model_index` indexes. So the frames genuinely are sitting
there, addressable by `model_index`, in a format the engine already parses.

**What breaks, in order:**

1. **Frame order lives only in the scene graph.** Which model belongs to which
   frame is the `_f` attribute on `nSHP` model references and `nTRN` frames.
   `Segment::from_vox` reads `models` and `palette` and nothing else, so the
   engine cannot tell frame 3 of the head from the left foot — in a multi-part
   animated scene the flat list interleaves both. (Recoverable offline —
   `voxlib.read_scene()` does exactly this, and `dot_vox` 5.2 exposes
   `ShapeModel::frame_index()` / `Frame::frame_index()` for the Rust side.)
2. **`model_index` is fixed at mesh-build time.** Every one of its ~40 uses is
   a field read off a RON spec inside a `bone_meshes()` implementation in
   `voxygen/src/scene/figure/load.rs`. There is no runtime variation anywhere.
3. **The mesh cache has no time dimension.** `FigureModelCache`'s key is
   `FigureKey { body, item_key, extra }` (`voxygen/src/scene/figure/cache.rs`)
   — no frame, no tick. The greedy mesh is built once per body+equipment combo
   and reused forever. Swapping `model_index` per frame would re-run the
   mesher (an async `SlowJobPool` job) and re-pack the atlas *every frame, per
   creature*. That is not an optimisation problem, it is a non-starter.

### What you would actually build instead

The mechanism already exists one level up, keyed on the wrong thing.
`FigureModelEntry<const N: usize>` (`voxygen/src/scene/figure/mod.rs:280`)
holds **N vertex ranges into one shared vertex buffer and one shared atlas**,
and `lod_model(lod)` picks one at draw time:

```rust
let model = if pos.distance_squared(cam_pos) > figure_low_detail_distance.powi(2) {
    model_entry.lod_model(2)
} else if … { model_entry.lod_model(1) } else { model_entry.lod_model(0) };
```

That is a plain integer selecting a sub-range — no re-mesh, no extra draw
call. A baked-frame feature is the same shape with the index coming from a
frame clock instead of camera distance: mesh all N frames **once** into one
buffer at load time, then advance an index per tick.

Cost, honestly: a new manifest field for the frame list, an `N`-way widening
of the figure model entry (it is already generic over `N`, but `N` is
per-body-kind and the LOD meaning is baked into the callers), a frame clock
per `FigureState`, and atlas budget multiplied by the frame count. That is a
real subsystem and a backlog row of its own, not a side effect of importing a
script.

**And it is probably the wrong thing to build.** Rigid bone transforms are why
one 15-part wolf animates for idle, walk, run, jump, swim and every attack.
Baked frames would need each of those hand-sculpted per frame per creature.
The only cases where baked frames genuinely beat bones are effects with **no
rigid skeleton at all** — a melting puddle, a dissolving corpse, a
squash-and-stretch impact — and reference 04's verdict already stands: those
belong to particles and shaders here, not to voxel meshes.

## The actual find: the scene graph as an authoring surface

The engine throws the `.vox` scene graph away. An **importer** must not,
because it holds information that exists nowhere else.

Run `voxlib.read_scene()` on `assets/voxygen/voxel/char_template.vox` — the
engine's own authoring reference, which no code loads — and it resolves to:

```
female                 model[0] t=(0, 1, 20)   size=(14, 8, 9)   min_corner=(-7, -3, 16)
female-0               model[1] t=(0, 4, 20)   size=(10, 1, 3)   min_corner=(-5,  4, 19)
female-10              model[2] t=(0, 0, 22)   size=(16, 14, 11) min_corner=(-8, -7, 17)
dark-0                 model[3] t=(3, 1, 2)    size=(5, 7, 4)    min_corner=( 1, -2,  0)
belt_dark              model[4] t=(0, 0, 9)    size=(10, 7, 2)   min_corner=(-5, -3,  8)
dark-0                 model[3] t=(-4, 1, 2)   size=(5, 7, 4)    min_corner=(-6, -2,  0)
shoulder_l_brown       model[5] t=(6, 0, 16)   size=(5, 7, 4)    min_corner=( 4, -3, 14)
shoulder_l_brown       model[5] t=(-6, 0, 16)  size=(5, 7, 4)    min_corner=(-8, -3, 14)
grayscale              model[6] t=(0, 0, 14)   size=(14, 7, 9)   min_corner=(-7, -3, 10)
hand_left              model[7] t=(-8, 0, 8)   size=(3, 3, 5)    min_corner=(-9, -1,  6)
hand_right             model[8] t=(7, 0, 8)    size=(3, 3, 5)    min_corner=( 6, -1,  6)
grayscale              model[9] t=(0, 0, 6)    size=(10, 7, 4)   min_corner=(-5, -3,  4)
```

Three things fall out of that, all verified:

**1. It is the only way to know which `model_index` is which part.** Twelve
models, and the names `head`/`hand_left`/`belt_dark` exist *only* in `nTRN`
`_name`. Reference 03 tells you to pack a creature into one file and select
parts with `model_index`; if that file came out of MagicaVoxel, this is how
you recover the mapping instead of bisecting it by trial and error.

**2. `_t` is the part's centre in the assembled figure — not a manifest
offset.** Note `hand_left` at x = −8 and `hand_right` at x = +7, symmetric
about the midline, while every midline part sits at x = 0. `Placement.min_corner(size)`
converts centre → the corner XYZI coordinates are relative to.

Do not confuse this with the manifest `offset`, which is a *different
quantity* in a *different space*: it positions the model relative to **its own
bone's origin**. The convention is confirmed exactly — `humanoid_armor_hand_manifest.ron`
uses `(-1.5, -1.5, -2.5)` for a hand, and `char_template`'s hand model is
`(3, 3, 5)`, i.e. `offset == -size/2`, the model centred on its bone.
`VoxModel.normalised()` already hands you this number; the scene graph is not
needed for it and must not be substituted for it.

What `_t` *is* good for is the **assembled rest pose** — proportions and
part placement in the same voxel-unit Z-up space the skeleton works in. That
is a useful cross-check and starting estimate when tuning a new species'
`SkeletonAttr` block (reference 03 §(b) — the ~16 tuning numbers, several of
which default silently). It is only an estimate: `SkeletonAttr` values are
**parent-relative** (`chest_mat = torso_mat * …`, `shoulder_l = chest_mat * …`
in `compute_matrices_inner`) and species-tuned, so they need the parent chain
subtracted and then hand-tuning. Some line up exactly — humanoid
`hand: (7.0, -0.25, 0.5)` against `hand_right` at x = 7 — and others do not.
Use it to start, not to finish.

**3. One model, placed twice, is the lateral convention in the file itself.**
`model[3]` and `model[5]` each appear at two mirrored translations. That is
reference 03's "one `.vox` serves both sides" rule, visible in Veloren's own
template.

## Using it

```python
import voxlib
models, palette = voxlib.read_vox("some_magicavoxel_export.vox")
for p in voxlib.read_scene("some_magicavoxel_export.vox"):
    size = models[p.model_id].declared_size
    print(p.name, "-> model_index:", p.model_id,
          "assembled centre:", p.translation,
          "min corner:", p.min_corner(size))
```

- Returns `[]` for any file with no scene graph — every file `voxlib` writes,
  and essentially every shipped asset in this repo (reference 01).
- Rotations (`_r`) are **deliberately not applied**. The engine meshes voxels
  on the grid and rotates via the bone matrix, so a part rotated in a
  MagicaVoxel scene has to be re-authored upright, not imported.
- `animation_frames(placements)` groups by `_f` for inspecting a
  MagicaVoxel-animated file. It is a reader only — see the section above for
  why the engine cannot play the result.
- `voxlib` still never *writes* a scene graph. The engine ignores it, and
  writing one would only create a second, divergent source of truth about
  part placement alongside the manifest.

## The recommended workflow, concretely

MagicaVoxel earns its place in this pipeline as a **sculpting and layout**
tool, not an animation one:

1. Sculpt or lay out the parts in MagicaVoxel's world editor, one object per
   skeleton bone, named after the bone, dragged into their assembled
   positions. This is genuinely better UX than typing coordinates — and it is
   demonstrably how `char_template.vox` was made.
2. Save the `.vox`. Run `read_vox` + `read_scene` to get the parts, their
   `model_index`, and their assembled placements.
3. Write the manifest rows: `offset` from `VoxModel.normalised()` (or
   `-size/2` for a centre-pivoted part), `model_index` from the scene graph.
4. Use the `_t` values as the opening bid for the `SkeletonAttr` numbers, then
   tune in-game with `hot-reloading` on (reference 03's feature table).
5. Verify through the engine's own loader, never a viewer (reference 05).

Anything parametric — symmetry, variants, rune counts, recolours — skip
MagicaVoxel entirely and generate it, which is what the rest of this skill is
about.
