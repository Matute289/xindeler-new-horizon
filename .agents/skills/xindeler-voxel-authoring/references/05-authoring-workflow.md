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
| `material_of(i)` / `intent` | what an index *is* vs what it was *asked* to be (drives `lint`) |

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
- `shell()` deletes every fully-enclosed voxel. Halves the voxel count of
  large props with no visible change.
- `normalised()` → `(shifted, size, original_min)`; `bounds()`; `len(model)`.

### `write_vox(path, models, palette) -> [origin per model]`

Writes `MAIN { SIZE+XYZI…, RGBA }` — the exact shape the engine reads. The
returned origins **are your manifest offsets**; the file itself stores no
position.

### `read_vox(path) -> (models, palette)`

Parses the same subset and **reserves every index the asset uses**, so you can
extend a shipped asset's palette without repainting it. Scene/`MATL`/`LAYR`
chunks in a MagicaVoxel-authored source are dropped on re-write — which
changes nothing about how the engine renders it.

### `lint(models, palette, humanoid=False)`

Catches the silent failures: empty models, out-of-range sizes, transparent
palette entries, *accidental* glowy/shiny indices (deliberate ones, allocated
via `add(..., GLOWY)`, are not flagged), index 16, humanoid material clashes,
and models large enough to be worth `shell()`-ing.

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
    pal = Palette(); core = pal.add(col, GLOWY); rim = pal.add((40,40,60))
    m = VoxModel().sphere((0,0,0), r, rim).shell().sphere((0,0,0), r-2, core)
    write_vox(f"assets/voxygen/voxel/object/arcane_orb_t{tier}.vox", [m], pal)
```

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

Then look at it in game — `cargo run --bin xindeler-voxygen` (on macOS add
`--no-default-features --features default-publish,shaderc-from-source,egui-ui`;
hot-reloading doesn't work there). Asset hot-reload *does* work, so you can
regenerate the `.vox` and retune manifest offsets without restarting.

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
| Terrain sprite | `sprite_manifest.ron` | `SpriteKind` variant |
| Dropped item | `item_drop_manifest.ron` | none (offset auto-centred) |

## Git LFS

`.vox` is LFS-tracked (`.gitattributes`) and blobs go to the **VPS**, never to
GitHub. Just `git add` normally — the pre-push hook routes the blob; GitHub
gets a pointer. Never add a workflow that does `actions/checkout` with
`lfs: true` against GitHub.

Per-asset size is small (a wolf head is ~1 KB) but a batch generator can add
hundreds of objects in one PR; prefer `model_index` packing when you're
generating a family, and `shell()` large props.

## Repo discipline

Standard for this repo: branch off the current working branch (check
`git branch --show-current`), one PR, run the specialist reviewers against the
diff **before** opening it, never merge. Design docs go to `docs/design/` via
its own branch + PR. Clean up throwaway generators and probe examples — don't
leave scratch code in the workspace.
