# 01 — The `.vox` format, and the slice of it the engine reads

Everything here was verified on 2026-09-13 by writing files with `dot_vox`
5.2.0 and `tools/voxel/voxlib.py`, reading real shipped assets back, and
loading both through the engine's own `Segment::from_vox`.

## The file, as the engine sees it

```
"VOX " <u32 version=150>
CHUNK "MAIN" content_len=0 children_len=N
  ├─ CHUNK "SIZE" <u32 x> <u32 y> <u32 z>        ─┐ one pair
  ├─ CHUNK "XYZI" <u32 count> (x,y,z,i) × count  ─┘ per model
  ├─ … repeat SIZE+XYZI per model …
  └─ CHUNK "RGBA" (r,g,b,a) × 256
```

Every chunk is `id[4] | content_len:u32 | children_len:u32 | content |
children`. Only `MAIN` has children; everything else is a leaf.

**That is the whole format you need.** `nTRN` / `nGRP` / `nSHP` (the scene
graph), `LAYR`, `MATL` and `IMAP` are parsed by `dot_vox` and then **never
read by the engine**. Confirmed by inspection of every consumer:
`common/src/figure/mod.rs`, `voxygen/src/scene/figure/load.rs`,
`voxygen/src/scene/terrain/mod.rs`, `voxygen/src/hud/item_imgs.rs`,
`voxygen/src/ui/img_ids.rs`, `common/src/terrain/structure.rs` — none of them
touch `.scenes`, `.layers`, `.materials` or `.index_map`.

Measured on a sample of real assets (one per category — an NPC part, a
hand-authored prop, the particle model, and the authoring template):

| Asset | models | scenes | layers | materials |
|---|---|---|---|---|
| `npc/wolf/male/head.vox` | 1 | 0 | 0 | 0 |
| `npc/wolf/male/torso_front.vox` | 1 | 0 | 0 | 0 |
| `object/citadel_arcane_cannon/sphere_barrel.vox` | 1 | 0 | 0 | 0 |
| `particle.vox` | 1 (1×1×1, one voxel) | 0 | 0 | 0 |
| `char_template.vox` | 12 | 30 | 8 | 256 |

`char_template.vox` is the exception that proves the rule: it is a MagicaVoxel
**working file** kept as an authoring reference (its scene nodes are named
`"female"`, `"female-0"`, `"female-10"`, `"hand_left"`, `"belt_dark"` …), and
it is not loaded as a figure by anything. If you author in MagicaVoxel you get
scene chunks for free and they're harmless; if you generate files in code,
**do not bother writing a scene graph** — it is dead weight the engine skips.

Dead weight to the *engine*, that is. It is not dead weight to an **importer**:
those node names are the only record of which of the 12 models is the head,
and the engine's loader needs exactly that as `model_index`. `voxlib.read_scene()`
recovers it — see `references/06-magicavoxel-interop.md`.

> Both forms round-trip: a hand-built two-model file with `scenes: vec![]` and
> a hand-built file with a full root-transform → group → named-transform →
> shape graph both reload identically through `dot_vox::load`. Tested.

## Coordinates

`Voxel { x: u8, y: u8, z: u8, i: u8 }`. Right-handed, **Z up**, matching
MagicaVoxel and the engine's `Vec3`. In-engine that becomes
`Segment::set(Vec3::new(x, y, z), cell)` with no axis swizzle
(`Segment::from_vox`, `common/src/figure/mod.rs`), so **+X right, +Y forward, +Z up** end to
end.

`SIZE` is `u32` and a file can claim `size.x = 300`, but voxel coordinates are
single bytes, so **no model can address past 255 on any axis**.

## Palette indexing — the off-by-one

The file stores 1-based colour indices (1..255); `dot_vox` subtracts 1 on read
and adds 1 on write, so:

```
in-memory index i  ──write──>  file byte i+1  ──read──>  in-memory index i
                                    │
                                    └─ MagicaVoxel shows this colour at slot i+1
```

`RGBA` entry `k` is the colour for in-memory index `k`. An index past the end
of the palette fails **silently, and differently per loader**:

- `Segment::from_vox` does `palette.get(voxel.i as usize)` and skips the voxel
  — it simply **disappears** (`common/src/figure/mod.rs`, the `from_vox` loop).
- `MatSegment::from_vox` (the humanoid path) falls back to `Rgb::broadcast(0)`
  — the voxel is **there and black**.

Always write all 256 entries. And note that an index which *is* in range but
was never assigned a colour is not an error either: it renders as whatever the
palette's fill is, usually opaque black. `voxlib.lint()` flags that case.

`voxlib.write_vox` handles the `+1` for you; you always work in in-memory
(engine) indices.

## Hard limits (all verified)

| Limit | Value | Where it bites |
|---|---|---|
| Coordinate range | 0..=255 per axis | `XYZI` stores `u8` |
| Palette index | 0..=**254** | `dot_vox`'s `write_vox` does `i + 1`; **`i = 255` panics** it with `attempt to add with overflow`. `voxlib` rejects it up front with a `ValueError` instead |
| Palette entries | write all 256 | missing entry ⇒ voxel dropped (`Segment`) or black (`MatSegment`) |
| **Figure/terrain** segment size | ≤ 512 per axis | `assert!`, `voxygen/src/mesh/segment.rs` (`generate_mesh_base_vol_figure`) |
| **Sprite** size | ≤ **32 × 32 × 64** | `assert!`, same file (`generate_mesh_base_vol_sprite`) — *panics the client*, and 64× tighter per axis than the figure cap |
| **Particle** model size | ≤ **16 × 16 × 64** | `assert!`, same file (`generate_mesh_base_vol_particle`) |
| `voxel_pos + manifest offset` | **[-128.0, +127.5]**, quantised to 0.5 | packed as 9 bits at half-voxel precision, `TerrainVertex::new_figure` in `voxygen/src/render/pipelines/terrain.rs`; the shader unpacks `(bits - 256.0) / 2.0` |
| Bones per figure | **16** | `anim::MAX_BONE_COUNT`; 4 bits in the vertex, `assert!(bone_idx <= 15)` |

Two of these bite differently from the rest:

- The 512³ figure cap is **unreachable from a single `.vox`** (coordinates are
  bytes, so ≤256) — it only matters for segments assembled by
  `DynaUnionizer`. The caps that will actually panic a generated asset are the
  **sprite** and **particle** ones, which are much tighter. `voxlib.lint(…,
  kind="sprite")` checks the right one for you.
- The ±128 limit is the *sum* of the voxel coordinate and the bone offset, so
  a 200-voxel-tall model with a −100 offset is already at the edge.

> Line numbers are deliberately omitted for upstream-owned files here — this
> is a Veloren-derived engine that merges `gitlab/master` periodically and
> they drift. Symbol names are stable; grep for them.

## The engine-side readers

```rust
// common/src/figure/mod.rs
Segment::from_vox(&DotVoxData, flipped: bool, model_index: usize,
                  custom_indices: Option<&HashMap<u8, Cell>>) -> Segment
Segment::from_vox_model_index(&DotVoxData, model_index, custom_indices)
Segment::from_voxes(&[(&DotVoxData, Vec3<i32> offset, bool xmirror)]) -> (Segment, origin)
MatSegment::from_vox(&DotVoxData, flipped, model_index) -> MatSegment
```

- `flipped` mirrors on X as `size.x - 1 - voxel.x`. This is how one limb model
  serves both sides in the `*_lateral_manifest.ron` specs — see reference 03.
- **`model_index` out of range returns a zero-sized segment, silently**
  (the `else` branch of `Segment::from_vox`). A typo'd index is an invisible body part, not an error.
- Assets are loaded through `common/assets`'s `DotVox` wrapper
  (the `DotVox` newtype in `common/assets/src/lib.rs`), addressed as dotted ids with
  `voxygen.voxel.` prepended by `graceful_load_vox`
  (`graceful_load_vox`, `voxygen/src/scene/figure/load.rs`). A missing file logs
  `"Could not load vox file for figure"` and substitutes
  `voxygen.voxel.not_found` — the pink error blob.

## Writing files

Two equivalent routes, both verified to produce files the engine loads:

**Python (preferred for authoring)** — `tools/voxel/voxlib.py`, no
dependencies:

```python
from voxlib import Palette, VoxModel, write_vox, GLOWY
pal = Palette()
stone = pal.add((120, 118, 130))
core  = pal.add((150, 90, 255), GLOWY)
m = VoxModel().sphere((0, 0, 0), 6, stone).sphere((0, 0, 0), 2, core)
origins = write_vox("out.vox", [m], pal)   # origins[0] is the manifest offset
```

**Rust (if you need the engine's own types in the same process)** — `dot_vox`
is already a dependency of `common`, `common/assets` and `voxygen`:

```rust
let data = dot_vox::DotVoxData {
    version: 150,
    index_map: Vec::new(),
    models: vec![dot_vox::Model { size, voxels }],
    palette,                 // 256 entries
    materials: vec![], scenes: vec![], layers: vec![],
};
data.write_vox(&mut std::fs::File::create(path)?)?;
```

Note `write_vox` emits no `IMAP` chunk, so `index_map` is write-only dead
state — pass `Vec::new()`.
