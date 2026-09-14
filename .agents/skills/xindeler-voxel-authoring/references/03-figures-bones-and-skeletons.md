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

## The one rule — now compiler-checked

**Array slot N of `bone_meshes()` ↔ the Nth `+`-marked bone declared in
`skeleton_impls!`.** By position. Never by name.

The RON field names (`head:`, `neck:`, `leg_fl:`) are just field names of the
per-species spec struct. The wiring is the *order of the array literal* inside
`make_vox_spec!` (the `make_vox_spec!` invocation for that body kind in
`load.rs`) matched against the `+` bone order in the skeleton (the
`skeleton_impls!` invocation in e.g.
`voxygen/anim/src/quadruped_medium/mod.rs`).

This used to have **no check of any kind** — get them out of sync and the head
rendered on the tail bone, with no warning at runtime or compile time. Three
guards now exist:

1. **`<Skeleton>::MESH_BONE_NAMES`** (`voxygen/anim/src/lib.rs`,
   `skeleton_impls!`) — every skeleton now publishes its `+` bone names as a
   const array, in mesh-slot order. This is the authority on what slot N means,
   and it is generated from the skeleton declaration itself, so it cannot drift
   from it.
2. **A mandatory `bones:` clause on `make_vox_spec!`**
   (`voxygen/src/scene/figure/load.rs`). Every invocation now names its
   skeleton and lists the bone order the mesh array was written against:

   ```rust
   make_vox_spec!(
       quadruped_medium::Body,
       bones: anim::quadruped_medium::QuadrupedMediumSkeleton [head, neck, jaw, tail,
           torso_front, torso_back, ears, leg_fl, leg_fr, leg_bl, leg_br, foot_fl,
           foot_fr, foot_bl, foot_br],
       struct QuadrupedMediumSpec { … },
       …
   );
   ```

   A `const` `assert!(anim::bone_names_eq(…))` compares that list to
   `MESH_BONE_NAMES` **at compile time**. Reorder either side and the build
   fails with the body kind, the skeleton, and an explanation — verified by
   deliberately swapping `neck`/`jaw` and watching `cargo check` reject it. It
   also asserts the list fits in `MAX_BONE_COUNT`, and `skeleton_impls!` now
   asserts the same for the skeleton itself.
3. **A load-time check for meshes past the last bone**
   (`debug_check_bone_slots`, `voxygen/src/scene/figure/cache.rs`). The array
   is always 16 slots wide no matter how many bones the skeleton has, so a
   mesh parked beyond the end is still *expressible* — it would be meshed into
   the atlas and then transformed by a bone matrix that is never written.
   Running once per figure model built, in dev builds only, it logs a
   `tracing::error!` naming the skeleton and the slot. (No `debug_assert` — it
   runs inside a slow-job closure on a pool with no panic handler, where a
   panic aborts the process rather than failing the job.)

   This is also the *only* check for the three `BodySpec` impls that bypass
   `make_vox_spec!` and therefore guard 2: `ship::Body` and `plugin::Body`
   (`load.rs`) and `VolumeKey` (`voxygen/src/scene/figure/volume.rs`). All
   three were verified by hand to fill only slots their skeleton has.

The one thing still **not** machine-checked is which expression sits in which
slot: the names are verified against the skeleton, but binding *this* array
element to *that* name is still positional convention. Note also that the names
in the three places need not agree — `golem`'s `mesh_torso_upper` fills the
`upper_torso` bone, `quadruped_small`'s `mesh_foot_fl` fills the `leg_fl` bone.
The `bones:` clause must use the **skeleton's** names, because that is what it
is checked against. See "Proposal: fully named mesh slots" at the end of this
file for the remaining gap and what closing it would cost.

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

For a part whose pivot is simply its own centre, the offset is exactly
`-size/2` — confirmed against `humanoid_armor_hand_manifest.ron`, whose
`(-1.5, -1.5, -2.5)` is `-size/2` of the `(3, 3, 5)` hand model.

**If the art came out of MagicaVoxel rather than `voxlib`**, its scene graph
records each part's name, its `model_index` and its placement in the
assembled figure — the only record of which model is the head. Read it with
`voxlib.read_scene()`; see `references/06-magicavoxel-interop.md`. Note that a
scene-graph `_t` is the part's **assembled-pose centre**, a different quantity
in a different space from the manifest `offset` above — useful as a starting
estimate for the `SkeletonAttr` numbers in §(b) below, never as the offset
itself.

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

⚠️ **`model_index` is not an animation frame.** It is read off the RON spec
inside `bone_meshes()` and baked into the cached mesh at build time; the
`FigureModelCache` key (`FigureKey { body, item_key, extra }`) has no frame or
tick component, so the mesh is built once and reused. A model list is a list
of *parts*, never a list of *poses*. `references/06-magicavoxel-interop.md`
works through why baked multi-frame animation does not fit this engine and
what would have to be built for it.

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
   `dimensions()`, `base_health()`, `base_poise()`, `mount_offset()` for the
   body kinds whose matches are exhaustive. The rest **default** — `mass()`
   falls through to `200.0` for a quadruped-medium and `threat_tier()` to `2`,
   so a forgotten species used to be a 200 kg tier-2 combatant with a clean
   build and no signal. **These are now caught by a test**, see
   "Fall-through audit" below.
4. `voxygen/anim/src/quadruped_medium/mod.rs` — 11 `SkeletonAttr` fields are
   exhaustive (`head`, `neck`, `jaw`, `tail`, `torso_front`, `torso_back`,
   `ears`, `leg_f`, `leg_b`, `feet_f`, `feet_b`); 5 more default
   (`scaler` → `0.9`, `startangle`, `tempo`, `spring`, `feed`), and
   `ears_for_trunk` isn't a match at all. Getting the defaulted ones wrong
   yields a wrong-sized creature with a wrong gait. Tuning numbers, not logic
   — and, again, **now caught by a test**.
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

## Fall-through audit — the silent defaults, now with a tripwire

The dangerous half of adding a species is the attributes that are **not**
exhaustive matches. There is no way for Rust to tell "explicitly 200 kg" from
"never given a mass", so the compiler cannot help. Two audits now do.

**How it works.** Every per-species wildcard arm that matters is wrapped in an
`attr_fallback!("<BodyKind>", "<attr>", <value>)` macro. The value and the
behaviour are unchanged; outside `cfg(test)` the macro expands to the value and
nothing else, so there is no branch, no atomic and no code in a shipped build
(it sits on a once-per-frame path, so that matters). Under `cfg(test)` it
records which body kind and attribute fell through. A test then walks the
entire creature roster — every species × body type of every body kind, plus
every `Body::Object` and `Ship` — and diffs the fall-throughs it sees against a
checked-in ledger.

| Side | Macro + recorder | Test | Ledger |
|---|---|---|---|
| Game data | `common/src/comp/body/attr_audit.rs` | `common/src/comp/body/attr_audit_test.rs` | `common/src/comp/body/attr_fallback_ledger.txt` |
| Animation | `voxygen/anim/src/attr_audit.rs` | `voxygen/anim/src/attr_audit_test.rs` | `voxygen/anim/src/attr_fallback_ledger.txt` |

**What this means for you.** Add a species and forget a value, and
`cargo test -p xindeler-common -p xindeler-anim --lib attr_audit` fails with the
species and attribute named, plus the exact ledger lines to add or delete.
**CI runs that exact command** on every PR to `development`/`main`
(`.github/workflows/ci-code-quality.yml`), so this is a real merge gate and not
a ritual. The pre-existing roster is grandfathered *by name*, so nothing
changed behaviourally — but the debt is now countable instead of invisible
(~880 game-data entries, ~700 animation entries at the time of writing). The
right response to a new `+` line is almost always to give the species an
explicit value, not to bless it into the ledger.

**Audited on the game-data side** (`common/src/comp/body/mod.rs`): `mass`,
`base_health`, `base_poise`, `base_energy`, `threat_tier` — the substantive
per-creature numbers. `scale`, `spacing_radius`, `magic_resist_tier` and
`combat_multiplier` are wrapped and label-checked but kept *out* of the ledger
via `DEFAULTS_BY_DESIGN`, because their catch-all is the neutral value
(`1.0` = no scaling, `None` = no innate magic resistance) rather than a guess,
and their matches are sparse by design — the source of `magic_resist_tier` says
so outright. Flipping one back to audited is a one-line change.

Both the per-species wildcards *and* the outer `match self` ones are wrapped,
so a body kind that has no species-level arm at all — an `Arthropod`'s
`base_poise`, a `Crustacean`'s `base_energy` — is covered too; those were the
biggest blind spot in the first cut of this audit.

**Explicitly out of scope**, and deliberately so: attributes whose catch-all
*is* the meaning (`immune_to`, `negates_buff`, `is_same_species_as`,
`localize_npc`, `humanoid_gender`), and everything already exhaustive and
compiler-enforced. `Body::Item` and `Body::Plugin` are not in the roster —
`Item`'s variants carry payloads so there is no flat list, and constructing a
`plugin::Body` in a unit test panics (its getters index a registry that is
empty without plugins loaded, a pre-existing sharp edge in
`common/src/comp/body/plugin.rs`). A `the_roster_reaches_every_body_kind` test
plus an exhaustive `body_kind` match make sure nothing *else* silently drops
out.

**Audited on the animation side**: the 40 `SkeletonAttr` fields across
`arthropod`, `biped_large`, `object`, `quadruped_low`, `quadruped_medium`,
`quadruped_small` and `ship` whose matches end in a wildcard. Every other
`SkeletonAttr` field is already exhaustive and needs no audit — the compiler
has it. The roster walks every body kind that has a `SkeletonAttr`, including
the ones with no wildcards today, so a fall-through added to one of them later
cannot pass vacuously.

Its `DEFAULTS_BY_DESIGN` exempts fields whose wildcard means *"this creature
has no such feature"* rather than *"nobody said what this creature's number
is"*: the twelve `biped_large` weapon-grip offsets (they position a held
weapon relative to the hand, not the creature), `BipedLarge.tail` → no tail,
`Arthropod.snapper` → `false`, `QuadrupedLow.side_head_{lower,upper}` → only
Hydra has side heads, and `QuadrupedSmall.lateral`. `Object.bone0`/`bone1`
stay audited on purpose — `Body::Object` is the two-bone escape hatch a new
prop or spell object takes, so that is exactly where you want to be asked.
Liveness tests assert that every `AUDITED_FIELDS` and every
`DEFAULTS_BY_DESIGN` entry is still a real fall-through, so neither list can
outlive its reason.

**Known scope limit, not yet closed:** the animation audit covers
`SkeletonAttr` only. The per-species `mount_point` / `mount_mat` tables (e.g.
`voxygen/anim/src/quadruped_medium/mod.rs`, and eight other body kinds) have
the same wildcard shape and are *not* audited — a new rideable species gets a
default saddle position silently. Same mechanism would fix it; it needs the
test to call those functions too, which their differing signatures make
fiddlier than one macro.

**Regenerating a ledger.** Run the test; the failure message is a
ready-to-paste `+`/`-` diff, or re-bless wholesale with
`ATTR_LEDGER_BLESS=1 cargo test -p xindeler-common -p xindeler-anim --lib attr_audit`.
Both ledgers are plain text, `#`-commented, grouped by `<BodyKind>.<attr>`.

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

The repo `CLAUDE.md` used to conflate the two, telling macOS readers to drop
`hot-reloading` for a bug that belongs to `hot-anim` — which would have turned
off exactly the thing an asset author wants, the `.vox`/manifest watcher
(`BodySpec::reload_watcher`) that lets you regenerate a model and retune
offsets without restarting the client. That was corrected in PR #326: a plain
`cargo run --bin xindeler-voxygen` keeps the watcher on, on every platform.

## Proposal: fully named mesh slots (not built — needs sign-off)

The `bones:` clause closes most of the geometry↔skeleton gap, but one step is
still convention: the *expression* in array slot N is not bound to the name in
position N of the clause. Someone can still write the head expression second
and the neck expression first, and both checks pass.

Closing that properly means replacing the bare array literal in each
`make_vox_spec!` block with labelled entries — the macro would build the array
from named slots and reject a name that is not the skeleton's, in the
skeleton's order:

```rust
|FigureKey { body, .. }, spec| bone_meshes! {
    head:  Some(spec.central.read().0.mesh_head(body.species, body.body_type)),
    neck:  Some(spec.central.read().0.mesh_neck(body.species, body.body_type)),
    …
}
```

**Why it is not done here.** It is a ~200-site mechanical rewrite inside a
6,300-line file that upstream Veloren edits regularly, on a code path shared by
every creature in the game. The value it adds over the existing compile-time
order check is real but incremental — it catches "right names, wrong
expression order", which is rarer than "the two lists drifted apart", the case
already covered. The cost is a large permanent merge-conflict surface against
`gitlab/master`. That trade needs Matías's call, not an agent's.

**If it is approved**, the shape above is the one to build: `make_vox_spec!`
already receives the skeleton type and the bone list, so `bone_meshes!` can be
generated inside it and the per-name check is a `const` string comparison
against `MESH_BONE_NAMES` exactly like the existing one. The migration is
mechanical (prefix each top-level array element with its bone name, in order)
and should be done one body kind per commit so a bad edit is bisectable.

A cheaper adjacent idea, worth doing first if the full migration is declined:
`voxlib.read_scene()` can already recover part names and `model_index` from a
MagicaVoxel-authored `.vox`'s scene graph (see `references/06`). A
`tools/voxel/` lint could compare a manifest's declared `model_index` order
against those names for the assets that *have* a scene graph, catching the
same class of mistake at authoring time without touching engine code at all.
Most shipped assets have no scene graph, so it is a partial check — but it is
free and it targets exactly the new-asset path this skill is about.

## Proposal: move per-species balance numbers into RON (not built — needs sign-off)

The fall-through audit above is a **tripwire around a design smell, not a cure
for it.** `mass`, `base_health`, `base_poise`, `base_energy`, `threat_tier`,
`combat_multiplier` and friends are designer-facing balance tables written as
Rust match arms in a shipped engine crate. Nerfing a Minotaur means editing
Rust and recompiling; there is no hot-reload and no data file a designer can
touch.

The repo already has the right container for this and uses it elsewhere:

- `common/src/comp/body/mod.rs`'s `AllBodies<BodyMeta, SpeciesMeta>` is already
  a `FileAsset`, keyed body kind → species.
- Each body module's `AllSpecies<SpeciesMeta>` is a **struct with one required
  field per species**, so a RON file of that shape *cannot omit a species*:
  serde fails at load, by name, with no test needed.
- `assets/common/npc_names.ron` already ships exactly this shape, and
  `assets/common/combat_tuning.ron` is the precedent for balance numbers in RON.

So the honest end-state is `assets/common/body_stats.ron` as
`AllBodies<BodyStatsDefaults, SpeciesStats>` — which gives the same guarantee
the audit spends ~1,600 ledger lines, a macro and two test modules to
approximate, for free, at load, with hot-reload and designer editability
thrown in. On that day the whole audit gets deleted.

**Why it is not done here.** It is a migration of every per-species number in
the game out of code and into data, touching balance for ~250 creatures, and it
needs a decision about which numbers are *content* (RON) versus *engine
behaviour* (code) that is Matías's to make, not an agent's. Treat both ledgers
as scaffolding with a known expiry, not a permanent register.

A smaller, strictly-good step available first: `threat_tier` and
`magic_resist_tier` are **Xindeler-authored, not inherited from upstream
Veloren**. For those two, deleting the wildcard and making the match exhaustive
is better than auditing it — upstream cannot conflict with a function it does
not have, a new species then fails the *build* rather than a test, and it drops
a large slice of the ledger.

## Note for upstream merges

`make_vox_spec!`'s `bones:` clause is mandatory, so a body kind that arrives
from a `gitlab/master` merge **will not compile** until someone adds its bone
list. That is the intended forcing function, not a broken merge — the error
message says exactly what to do. Worth remembering when running the
`gitlab-master-merger` skill.
