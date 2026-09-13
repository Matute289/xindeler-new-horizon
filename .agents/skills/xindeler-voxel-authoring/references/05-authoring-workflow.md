# 05 — The authoring workflow

## `tools/voxel/voxlib.py`

Self-contained, no third-party dependencies, Python 3.9+. Run
`python3 tools/voxel/selftest.py` to confirm it still round-trips.

```python
import sys; sys.path.insert(0, "tools/voxel")
from voxlib import Palette, VoxModel, write_vox, read_vox, lint, MATTE, SHINY, GLOWY
```

### `Palette`

| Call | Does |
|---|---|
| `add(color, material=MATTE)` | allocates (or reuses) a legal index; `MATTE` from 22 up, `SHINY` 8–12, `GLOWY` 13–15; never returns a claimed index |
| `set(index, color, material=None)` | exact index, for hollow markers or matching an existing asset |
| `reserve(indices)` | mark indices in use so `add` skips them |
| `material_of(i)` | what material an index *is*, by range |
| `intent` | what each index was *asked* for — this is what lets `lint` tell a deliberate glow from an accidental one |

### `VoxModel`

Sparse `{(x,y,z): index}`, **+X right, +Y forward, +Z up**. Coordinates may go
negative while you build.

- Primitives: `set` / `clear` / `get`, `box(lo, hi, i)`,
  `sphere(c, r, i, hollow=False)`, `ellipsoid(c, (rx,ry,rz), i, hollow)`,
  `cylinder(base, r, h, i, axis="z")`, `capsule(a, b, r, i)`,
  `cone(base, r, h, i)`, `torus(c, major, minor, i)`, `line(a, b, i)`.
- `capsule` is the workhorse for limbs, tails, horns, tentacles — a swept
  sphere between two points.
- `torus` is the workhorse for rune rings, halos and orbit bands.
- Transforms: `translate`, `mirror_x(plane=None, merge=True)`,
  `recolor({old: new})`, `map_indices(fn)`, `union(other, offset)`.
- `shell()` deletes every fully-enclosed voxel. ⚠️ **It does not make a model
  cheaper to render — it makes it more expensive.** The greedy mesher emits a
  quad at every filled↔empty boundary with no visibility culling
  (`should_draw_greedy`, `voxygen/src/mesh/segment.rs`), so hollowing a
  solid volume adds a whole second inward-facing surface. Interior voxels of a
  solid model are already free at render time. Use `shell()` only when the
  inside is meant to be seen, or to cut file size on a very large prop and
  accept the extra faces. The same caveat applies to `sphere(..., hollow=True)`
  and `ellipsoid(..., hollow=True)`.
- `normalised()` → `(shifted, size, original_min)`; `bounds()`; `len(model)`.

### `write_vox(path, models, palette) -> [origin per model]`

Writes `MAIN { SIZE+XYZI…, RGBA }` — the exact shape the engine reads. The
returned origins **are your manifest offsets**; the file itself stores no
position.

### `read_vox(path) -> (models, palette)`

Parses the same subset and **reserves every index the asset uses**, so you can
extend a shipped asset's palette without repainting it. It also records the
source file's declared `SIZE` on each model, and `normalised()` preserves it
while the edits still fit — so re-writing a shipped asset does **not** shift
it. That matters because several shipped assets don't start at their min
corner (`portal.vox` begins at `(1,1,0)`), and a silent shift would desync the
model from the manifest offset it was tuned against. Move a model deliberately
(`translate`, or edits outside the declared box) and it re-bases to its real
content, returning a non-zero origin you must fold into the offset.

Scene/`MATL`/`LAYR` chunks in a MagicaVoxel-authored source are dropped on
re-write — the engine never read them, so the render is unchanged.

### `lint(models, palette, humanoid=False, kind="figure")`

Catches the silent failures: empty models, indices that were **never allocated
in this palette** (they render as the palette fill, usually black — the most
common mistake), transparent palette entries, *accidental* glowy/shiny indices
(deliberate ones, allocated via `add(..., GLOWY)`, are not flagged), index 16,
humanoid material clashes, and models large enough to be worth reconsidering.

`kind` selects which mesher's size ceiling to check — `"figure"` (default),
`"terrain"`, `"sprite"` (32×32×64) or `"particle"` (16×16×64). Those are hard
`assert!`s that *panic the client*, and the sprite/particle ones are much
tighter than the figure one, so pass the right `kind` for where the asset is
going.

## Recipes

**A creature part with its pivot at the hip**

```python
pal = Palette(); hide = pal.add((92, 74, 58))
leg = VoxModel(name="leg_fr").capsule((0, 0, 0), (0, 0, -10), 2.0, hide)
_, size, origin = leg.normalised()
# origin == (-2, -2, -12)  ->  manifest: offset: (-2.0, -2.0, -12.0)
```

**A symmetric prop** — build one half, mirror it:

```python
rune = VoxModel().torus((0, 0, 0), 14, 1.5, core)
for k in range(3):
    rune.box((11 + k, -1, 0), (16 + k, 1, 0), core)
rune.mirror_x(plane=0)      # spokes on both sides for free
```

For a left/right **pair of limbs**, do *not* mirror here — author one and let
the `*_lateral_manifest.ron` spec mirror it (reference 03).

**A family of variants from parameters**

```python
for tier, (r, col) in enumerate([(4, (90,140,255)), (6, (160,90,255)), (8, (255,90,140))]):
    pal = Palette()
    core = pal.add(col, GLOWY)              # emissive heart
    glass = pal.add((180, 210, 255), SHINY) # renders at alpha 0.1, so the core shows
    m = VoxModel().sphere((0, 0, 0), r, glass).shell()   # a legitimate hollow: you see in
    m.sphere((0, 0, 0), max(r - 3, 1), core)
    write_vox(f"assets/voxygen/voxel/object/arcane_orb_t{tier}.vox", [m], pal)
```

An opaque outer layer would simply hide the core — remember the palette index
is the material, so a matte rim is a solid wall.

**A palette-exact recolour of a shipped asset**

```python
models, pal = read_vox("assets/voxygen/voxel/npc/wolf/male/head.vox")
glow = pal.add((255, 60, 60), GLOWY)     # safe: used indices are reserved
models[0].recolor({7: glow})
write_vox("assets/voxygen/voxel/npc/direwolf/male/head.vox", models, pal)
```

**Packing a whole creature into one file**

```python
origins = write_vox("assets/voxygen/voxel/npc/x/male/x.vox", [head, neck, jaw, torso], pal)
# -> manifest rows with model_index: 0,1,2,3 and offset: origins[i]
```

## The verify loop — do not skip this

Every `.vox` mistake in this engine is silent. Check with the engine's own
loader, not a viewer.

Write a throwaway example, run it, **delete it**:

```rust
// common/examples/vox_probe.rs   (temporary — do not commit)
use xindeler_common::figure::{CellSurface, Segment};
use xindeler_common::vol::{IntoFullVolIterator, SizedVol};

fn main() {
    let d = dot_vox::load(&std::env::args().nth(1).unwrap()).expect("load failed");
    for mi in 0..d.models.len() {
        let s = Segment::from_vox_model_index(&d, mi, None);
        let filled = s.full_vol_iter().filter(|(_, c)| c.is_filled()).count();
        let glowy = s.full_vol_iter()
            .filter(|(_, c)| c.get_surf() == Some(CellSurface::Glowy)).count();
        println!("model[{mi}] size={:?} filled={filled} glowy={glowy}", s.size());
    }
}
```

```bash
VELOREN_ASSETS="$(pwd)/assets" cargo run -q -p xindeler-common --example vox_probe -- out.vox
```

Assert what you designed: the size, the voxel count, and that exactly the
voxels you meant to glow come back as `Glowy`. A zero-sized segment means a
bad `model_index`; a voxel count lower than your generator's means dropped
voxels (palette entry missing).

Then look at it in game — `cargo run --bin xindeler-voxygen`. Keep the
`hot-reloading` feature on if you can: it is the **asset** watcher
(`assets_manager`), not the dylib code reload, so regenerating a `.vox` or
editing a manifest offset updates the running client. The repo CLAUDE.md's
macOS command drops it for a reason that actually applies to `hot-anim` —
see reference 03's hot-reload table before you copy that command while
iterating on art.

## Where files go

Per `assets/voxygen/voxel/README.md`:

| Directory | Contents |
|---|---|
| `npc/<species>/<body_type>/` | everything with a `Body` that isn't Humanoid / Object / ItemDrop |
| `object/` | `Body::Object` models that aren't projectiles or shared with sprites |
| `weapon/` | hand-slot items, projectiles, weapon components |
| `figure/` | `Body::Humanoid` parts (head, hair, eyes, beard, accessory) |
| `armor/`, `glider/`, `lantern/`, `item/` | equipment by slot |
| `sprite/` | terrain sprites — **always** put a shared model here, it's the narrowest category |

Asset ids in RON are dot-separated and rooted at `assets/voxygen/voxel/`:
`"npc.grolgar.male.head"` → `assets/voxygen/voxel/npc/grolgar/male/head.vox`.

## Manifest wiring, by asset type

| Asset | Manifest | Rust needed |
|---|---|---|
| Weapon | `biped_weapon_manifest.ron`, keyed by item definition id | **none** |
| Reskin of an existing NPC species | `<kind>_central_manifest.ron` + `_lateral_manifest.ron` | **none** |
| New species of an existing kind | same two manifests, Male **and** Female rows | `Species` variant + ~16 `SkeletonAttr` arms + `npc_names.ron` (reference 03) |
| Object / prop / VFX / projectile | `object_manifest.ron` (`bone0`/`bone1`, `model_index`, `custom_indices`) | `object::Body` variant arms |
| Terrain sprite | `sprite_manifest.ron` | `SpriteKind` variant — ⚠️ **not a peer of the rows above**: its discriminant is `(category << 16) \| id` baked into terrain block data, so adding one is a persisted chunk-format change. Also capped at 32×32×64 |
| Dropped item | `item_drop_manifest.ron` | none (offset auto-centred) |

## Git LFS

`.vox` is LFS-tracked (`.gitattributes`) and blobs go to the **VPS**, never to
GitHub. Just `git add` normally — the pre-push hook routes the blob; GitHub
gets a pointer. Never add a workflow that does `actions/checkout` with
`lfs: true` against GitHub.

Per-asset size is small (a wolf head is ~1 KB) but a batch generator can add
hundreds of objects in one PR; prefer `model_index` packing when you're
generating a family. Keep props solid: hollowing them costs faces, not saves them.

## One thing generators must not do

`voxlib` scripts live in `tools/`, which is dev tooling — not a second source
of truth for game data. A generator that hardcodes spell radii, tier
thresholds or damage numbers to derive its geometry duplicates balance data
that belongs in `assets/common/**.ron`. Either read the RON at generation
time, or keep the parameterisation purely cosmetic (segment counts, colour
ramps, silhouette proportions). See the `game-architecture` skill if you're
unsure which side of that line something is on.

## Repo discipline

Standard for this repo: branch off the current working branch (check
`git branch --show-current`), one PR, run the specialist reviewers against the
diff **before** opening it, never merge. Design docs go to `docs/design/` via
its own branch + PR. Clean up throwaway generators and probe examples — don't
leave scratch code in the workspace.
