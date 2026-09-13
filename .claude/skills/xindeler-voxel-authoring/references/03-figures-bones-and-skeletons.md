# 03 — Figures, bones, offsets, and what "reuse a skeleton" really costs

## The chain, end to end

```
assets/voxygen/voxel/**/*.vox
        │  graceful_load_segment(name, model_index)        [figure/load.rs]
        ▼
   Segment  (Dyna<Cell>)  ── paired with a Vec3<f32> offset ──> BoneMeshes
        │  make_vox_spec! closure returns [Option<BoneMeshes>; 16]
        ▼
   FigureModelCache::get_or_create_model                   [figure/cache.rs]
        │  for each array slot i: generate_mesh(.., offset, bone_idx = i)
        ▼
   one greedy mesh in a shared atlas, every vertex tagged with its 4-bit bone_idx
        │
        ▼
   Skeleton::compute_matrices -> [FigureBoneData; 16]      [anim/src/lib.rs]
        │
        ▼
   figure-vert.glsl:  f_pos = bones[bone_idx].bone_mat * pos
```

`pub type BoneMeshes = (Segment, Vec3<f32>);` — `load.rs`.

## The one rule that has no compiler check

**Array slot N of `bone_meshes()` ↔ the Nth `+`-marked bone declared in
`skeleton_impls!`.** By position. Never by name.

The RON field names (`head:`, `neck:`, `leg_fl:`) are just field names of the
per-species spec struct. The wiring is the *order of the array literal* inside
`make_vox_spec!` (the `make_vox_spec!` invocation for that body kind in `load.rs`) matched
against the `+` bone order in the skeleton (the `skeleton_impls!` invocation in e.g.
`voxygen/anim/src/quadruped_medium/mod.rs`). Get them out of sync and
the head renders on the tail bone — no warning, no error, at runtime or
compile time. If you change either list, diff them side by side.

`None` in a slot means "this bone has no mesh"; the array is padded to 16.

## Bone counts, per body kind

| Skeleton | mesh bones | bone list |
|---|---|---|
| `CharacterSkeleton` | 16 | head, chest, belt, back, shorts, hand_l, hand_r, foot_l, foot_r, shoulder_l, shoulder_r, glider, main, second, lantern, hold |
| `QuadrupedMediumSkeleton` | 15 | head, neck, jaw, tail, torso_front, torso_back, ears, leg_fl/fr/bl/br, foot_fl/fr/bl/br |
| `BipedLargeSkeleton` | 16 | head, jaw, upper_torso, lower_torso, tail, main, second, shoulder_l/r, hand_l/r, leg_l/r, foot_l/r, hold |
| `ObjectSkeleton` | **2** | bone0, bone1 |
| `PluginSkeleton` | 16 | bone0…bone15, **flat** (no hierarchy) |

Skeletons also declare *helper* bones without the `+` marker (`torso`,
`control`, `control_l/r`, `mount`, …). Those exist only on the CPU as
intermediate transforms and never receive a mesh.

## Offsets and pivots — where to put your voxels

`offset` is in **voxel units** (`f32`, so half-voxel placement works) and is
added to every voxel position *before* the bone matrix is applied:

```rust
// voxygen/src/mesh/segment.rs, generate_mesh_base_vol_figure
TerrainVertex::new_figure(atlas_pos, (pos + offs) * scale, norm, bone_idx)
```

Consequence: **the bone's pivot is local `(0,0,0)` after the offset.** To make
a leg rotate about its hip, choose `offset` so the hip voxel lands at
`(0,0,0)` — i.e. `offset ≈ -(pivot coordinate inside the .vox model)`. That is
why almost every offset in the shipped manifests is negative.

The manifests carry hand-written pivot conventions worth copying:

- `quadruped_medium_central_manifest.ron` (grolgar `head`) —
  `offset: (-7.0, -11.0, -8.0), //value in y dimension is full length of model`
- `quadruped_medium_lateral_manifest.ron` (grolgar `foot_fl`) —
  `offset: (-2.5, -4.5, -8.0), //y pivot should be -1/4 of the y dimension of the model`
- the same file's header — `//these are done very case by case`

**Workflow that makes this painless:** build every part in `voxlib` with the
pivot at the origin and let coordinates go negative. `VoxModel.normalised()`
returns `(shifted_model, size, original_min)`; `write_vox` returns the same
`original_min` per model. That vector *is* your manifest offset. No guessing.

Scale is applied by the skeleton, not the mesh — `base_mat *
Mat4::scaling_3d(s_a.scaler / 11.0)` for quadruped-medium
(`compute_matrices_inner`, `voxygen/anim/src/quadruped_medium/mod.rs`), `/8.0` for biped_large,
`BASE_HEIGHT * scaler * (1.0/25.0)` for characters. So **≈11 voxels ≈ 1 world
metre for a quadruped-medium**; match the existing species' proportions or
your creature will be the wrong size regardless of its `dimensions()`.

Remember the packing limit from reference 01: `voxel_pos + offset` must stay
within **[-128.0, +127.5]** on every axis.

## Central vs lateral manifests

An **axial split, not a hierarchy split**:

- `*_central_manifest.ron` — midline parts, one each, never mirrored: head,
  neck, jaw, ears, torso_front, torso_back, tail. RON field: `central:`.
- `*_lateral_manifest.ron` — paired left/right limbs. RON field: `lateral:`.

The payoff is that **one `.vox` serves both sides**: the left-side loader
passes `flipped = true` and the engine mirrors on X. `grolgar`'s `leg_fl` and
`leg_fr` both point at `"npc.grolgar.male.leg_fr"`. Each side still gets its
own `offset` row, because mirroring flips the model but not its pivot — for a
symmetric limb the two offsets happen to match (grolgar's front legs), while
its `leg_bl`/`leg_br` show the case where they differ.

So: **do not generate mirrored left/right pairs as separate files.** Generate
one, let the manifest mirror it. Use `voxlib`'s `mirror_x()` only when you
want a single *symmetric* model (a rune ring, a skull, a shield).

## One file, many parts: `model_index`

A whole creature can live in one `.vox` as N models, selected per bone:

```ron
neck:        ( offset: (-3.0, -4.5, -9.5), central: ("npc.llama.male.llama"), model_index: 1, ),
jaw:         ( offset: (-2.0,  0.0, -2.0), central: ("npc.llama.male.llama"), model_index: 2, ),
torso_front: ( offset: (-6.0, -9.0, -6.0), central: ("npc.llama.male.llama"), model_index: 3, ),
```

Used by `quadruped_{small,medium,low}`, `golem`, `biped_large`, `crustacean`
and `object` manifests. `write_vox(path, [part0, part1, …], pal)` produces
exactly this layout and returns the per-model offsets in the same order.

Trade-off: one file is tidier for a generator and halves the LFS object count,
but a per-part file lets you regenerate one limb without touching the others,
and matches the majority of shipped assets. Either is fine; be consistent
within a creature.

Two RON shapes exist for naming a model, depending on the body kind:

```ron
// Shape A — VoxSpec tuple: (path, [x,y,z] offset, optional model_index)
vox_spec: ("armor.misc.chest.grayscale", (-7.0, -3.5, 2.0)),
// Shape B — named struct (all NPC body kinds)
head: ( offset: (-7.0, 0.0, -9.0), central: ("npc.grolgar.male.head"), model_index: 0 ),
```

Omitted parts default to `armor.empty` rather than erroring
(`VoxSimple::default`), so a missing row is an invisible limb, not a crash.

## "Start from existing skeletons and attach our own `.vox` skins" — verdict

Matías's framing is **half right**, and the halves matter:

### (a) Reskinning an existing species — genuinely zero Rust ✅

Point the existing manifest rows at your new `.vox` files, retune the offsets.
You inherit every animation that body kind already has. Manifests are
hot-reloadable (`BodySpec::reload_watcher`), so the iteration loop is fast.

The constraint is **topology**: your art must be cut into exactly the parts
that skeleton expects (15 pieces for a quadruped-medium), and the proportions
must suit the existing `SkeletonAttr` numbers or the gait will look wrong.
This is the cheapest way to ship a new-looking monster and should be the
default.

### (b) A new *species* of an existing body kind — small but nonzero Rust ⚠️

No new animation functions, but several matches are exhaustive, so a new enum
variant won't compile until you fill them. **The dangerous half is the ones
that are *not* exhaustive** — those compile clean and give you a wrong
creature. Traced against `ClaySteed`, the most recent addition:

1. `common/src/comp/body/quadruped_medium.rs` — `Species` variant, the
   `AllSpecies` field, the `Index` arm. ⚠️ **Append at the end of the enum.**
   The explicit `= N` discriminant is not the wire tag: serde encodes by
   positional variant index, and rtsim persists `comp::Body` through
   `rmp_serde`, so inserting or reordering silently breaks existing saves.
   (Character/pet persistence is different again — it stores the species *by
   name* via `to_string`/`from_str`, so adding needs no migration but
   *renaming* does. The humanoid path is the exception to the exception: it
   stores `species as u8` and reads it back by index into `ALL_SPECIES`.)
2. `assets/common/npc_names.ron` — a matching entry. `AllSpecies` derives a
   plain `Deserialize` with no `serde(default)` and `NPC_NAMES` is a
   `load_expect` `lazy_static`, so a missing entry panics on the first
   NPC-name lookup.
3. `common/src/comp/body/mod.rs` — **compiler-enforced** (you cannot forget):
   `dimensions()`, `base_health()`, `base_poise()`, `mount_offset()`.
   **Silently defaulted** (you must fill these deliberately): `mass()` falls
   through to `200.0` and `threat_tier()` to `2`, so a forgotten species is a
   200 kg tier-2 combatant with a clean build.
4. `voxygen/anim/src/quadruped_medium/mod.rs` — 11 `SkeletonAttr` fields are
   exhaustive (`head`, `neck`, `jaw`, `tail`, `torso_front`, `torso_back`,
   `ears`, `leg_f`, `leg_b`, `feet_f`, `feet_b`); 5 more silently default
   (`scaler` → `0.9`, `startangle`, `tempo`, `spring`, `feed`), and
   `ears_for_trunk` isn't a match at all. Getting the silent ones wrong yields
   a wrong-sized creature with a wrong gait and no warning. Tuning numbers,
   not logic.
5. Both `*_manifest.ron` files — rows for **Male and Female**, or the figure
   logs `"No head specification exists for the combination of …"` and falls
   back to `not_found`.
6. The `.vox` files.

Optional: `creature_type.rs`, spawn configs, `ability_set_manifest.ron`,
loadouts, i18n names, world spawn tables.

### (c) A new *body kind* — a real subsystem 🚫 for a one-off

A new `Body` variant **appended at the end** (same serde-index rule as above)
with arms in every method
in `common/src/comp/body/mod.rs`; a new body module; a new skeleton module
with `skeleton_impls!`, `compute_matrices_inner`, `SkeletonAttr` and at
minimum `idle`/`run`/`jump` animations; a new `make_vox_spec!`; a new
`FigureModelCache` + `FigureState` map and ~7 match arms in
`voxygen/src/scene/figure/mod.rs`; new manifests; and every exhaustive `Body`
match across server/persistence/rtsim/agent. Only do this for a genuinely new
class of creature, and plan it as its own backlog row.

### Escape hatch: `Body::Object` (2 bones)

For a prop, a projectile, a turret or a spell object, `Body::Object` needs no
skeleton work at all: a `Body` variant, a two-bone row in
`object_manifest.ron`, and you get an animated figure with lighting, shadows,
`visual_scale()`, `custom_indices` and a `DeleteAfter` lifetime. This is the
path the Cromatolis Aerial Citadel took (COW-8). See reference 04.

## Weapons

A weapon is a **real bone**, not an attachment: `main` and `second` are mesh
bones of `CharacterSkeleton` (slots 12 and 13) and `BipedLargeSkeleton`. Their
voxels are meshed into the same atlas as the body and carry `bone_idx = 12`.
Attachment is pure matrix parenting: `main_mat = control_l_mat *
Mat4::from(self.main)`.

Which model is chosen comes from `ToolKey` (the item's definition id, or a
modular-weapon key) looked up in `assets/voxygen/voxel/biped_weapon_manifest.ron`
— shared by humanoid, biped_large and biped_small. Off-hand mirroring recomputes the
offset: `offset.x = -offset.x - segment.sz.x`.

So authoring a new weapon is: one `.vox` under
`assets/voxygen/voxel/weapon/<class>/`, one `vox_spec` row keyed by the item
id, done. **No Rust, no new bone.** This is the single easiest category to
hand-author.

## Hot-reload: two different features, often confused

These are **separate cargo features** and it matters a great deal to an asset
author which one you have:

| Feature | What reloads | In `default`? |
|---|---|---|
| `hot-reloading` → `common/hot-reloading` → `assets_manager/hot-reloading` | **assets**: `.vox` files and RON manifests | ✅ yes |
| `hot-anim` → `anim/use-dyn-lib` → `common/dynlib` | **animation code**, as a runtime-built dylib | ❌ no |
| `hot-egui` → `voxygen-egui/use-dyn-lib` → `common/dynlib` | the egui overlay's code | ❌ no |

`common/dynlib` shells out to `cargo rustc --crate-type dylib -Z
unstable-options` (hence the nightly requirement) and **does not work on
macOS** (`common/dynlib/src/lib.rs` logs `"The hot reloading feature does not work on macos."` and gives up). So
animation-*code* iteration on macOS means a rebuild — but that was never in a
default build anyway.

⚠️ **The repo CLAUDE.md's macOS run command conflates the two**: it drops
`hot-reloading` citing the `common/dynlib` macOS failure, but `hot-reloading`
is the *asset* feature and doesn't touch `common/dynlib` at all. Dropping it
turns off exactly the thing an asset author wants — the `.vox`/manifest
watcher (`BodySpec::reload_watcher`) that lets you regenerate a model and
retune offsets without restarting the client. Try keeping `hot-reloading` on
while iterating on art; if the client misbehaves on macOS for an unrelated
reason, fall back to the documented command and restart between iterations.
