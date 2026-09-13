# 02 — Palette indices are materials. Read this before placing a voxel.

The colour of a voxel comes from the palette entry. **What kind of surface it
is comes from the index number itself.** Two voxels with identical RGB behave
completely differently depending on which slot that RGB lives in.

Source of truth: `Cell::from_index` in `common/src/figure/cell.rs:100-114` and
`MatSegment::from_vox` in `common/src/figure/mod.rs:252-269`. Both were
verified by round-tripping one voxel per index 0..24 through the real engine
loader.

## The table (in-engine, 0-based indices)

| Index | Result via `Segment` (everything except player humanoids) |
|---|---|
| **0–7** | Matte |
| **8–12** | **Shiny** — `render_alpha = 0.1`, plus a cheap reflection hack |
| **13–15** | **Glowy** — `emitted_light += 20 × colour`; feeds the bloom chain |
| **16** | **Hollowing** — draws *nothing*, and *deletes* whatever an earlier-layered segment put at that position |
| **17–21** | Matte, and **carve-proof**: immune to another segment's index-16 |
| **22–254** | Matte |
| **255** | **Unusable** — `write_vox` panics (`i + 1` overflows `u8`) |

MagicaVoxel's own palette is 1-based, so **engine index `i` is MagicaVoxel
slot `i + 1`**. `assets/voxygen/voxel/sprite_manifest.ron:1-22` says the same
thing in its header comment — but note that comment claims *"16-255: Matte"*,
which is **stale**: index 16 hollows and 17–21 are carve-proof. Trust the code
and this table.

## The humanoid trap

`Body::Humanoid` art is loaded through `MatSegment`, not `Segment`, so the low
indices mean something else entirely:

| Index | Material, recoloured at runtime from `humanoid_color_manifest.ron` |
|---|---|
| 0 | `Skin` |
| 1 | `Hair` |
| 2 | `EyeDark` |
| 3 | `EyeLight` |
| 4 | `SkinDark` |
| 5 | `SkinLight` |
| 6 | *(was `Clothing`, commented out — behaves as a normal colour)* |
| 7 | `EyeWhite` |
| 8+ | falls through to the `Segment` table above |

So a humanoid head, body, hair or beard `.vox` **must** use 0–7 for the parts
that should track the player's chosen skin/hair/eye colour, and must **not**
use them for anything else. A non-humanoid creature using index 0 is fine —
the shipped `citadel_arcane_cannon` models use 0–7 throughout.

Armour has a *different* recolouring mechanism again: `recolor_grey`
(`voxygen/src/scene/figure/load.rs:117-129`) tints only voxels where
`R == G == B`, around a neutral of 178. That is why the shipped armour assets
are named `*grayscale`. If you generate armour, author it in greys and let the
manifest's `color: Some((r,g,b))` do the tinting.

## Hollowing — how layered parts carve each other

Only meaningful when several segments are combined with
`DynaUnionizer::unify_with`, which today is the humanoid head
(`load.rs:393-421`: head + eyes + hair + beard + accessory + helmet). The rule
it implements:

```
if old is override-hollow (17–21) -> keep old        // "you can't carve me"
else if new is hollowing (16)     -> erase to empty  // hat deletes hair
else if new is filled             -> new wins        // normal layering
else                              -> keep old
```

Practical use: a helmet model paints index 16 where the hair should be removed;
a face that must never be carved uses 17–21.

If you are authoring a single standalone model (a creature part, a weapon, a
VFX prop), index 16 is simply an invisible voxel — a waste of a voxel, and a
bug if you meant a colour. `voxlib.lint()` flags it.

## Fire, Water and SwirlyCrystal — only via `custom_indices`

`CellSurface` has three more variants that **`Cell::from_index` can never
produce**: `Fire = 3`, `Water = 4`, `SwirlyCrystal = 5`. They are reachable
only by passing `custom_indices: HashMap<u8, Cell>` to the loader, which in
practice means declaring them in the RON manifest:

```ron
CampfireLit: (
    bone0: (
        offset: (-9.0, -10.0, 0.0),
        central: ("object.campfire_lit"),
        custom_indices: {
            14: (attr: Fire),
            15: (attr: Fire),
            129: (attr: Fire),
        },
    ),
    ...
)
```

`custom_indices` is supported by **`object_manifest.ron`** (every
`Body::Object`, via `ObjectCentralSubSpec`, `load.rs:5990-5997`) and by
**`sprite_manifest.ron`** (`voxygen/src/scene/terrain/sprite.rs:30`). It is
**not** available on NPC central/lateral specs, humanoid armour, or weapons —
those structs have no such field.

What each does in the shader (`assets/voxygen/shaders/include/light.glsl:190-230`):

| Surface | Shader behaviour |
|---|---|
| `Glowy` (1) | `emitted_light += 20 × colour` — flat, constant emission |
| `Shiny` (2) | `render_alpha = 0.1` — near-transparent, fake reflection |
| `Fire` (3) | emissive **and time-animated**: `5×colour`, modulated by `noise_3d(…, tick)` — a live flicker, no CPU cost |
| `Water` (4) | `render_alpha = 0.2`, `render_mat = MAT_PUDDLE` |
| `SwirlyCrystal` (5) | emissive, **colour-cycling over `tick`** between two violet/cyan poles, plus a pulse |

**This is the cheapest animation in the entire engine.** `Fire` and
`SwirlyCrystal` give a static `.vox` a convincing live shimmer for the cost of
one RON line, with no skeleton, no per-frame Rust and no particles. For spell
props (orbs, braziers, crystals, runes) reach for these before you reach for a
bone animation. See reference 04.

## Allocating a palette in code

`voxlib.Palette` keeps you inside the legal ranges:

```python
pal = Palette()
stone = pal.add((120, 118, 130))          # -> first free matte slot, from 22 up
core  = pal.add((150, 90, 255), GLOWY)    # -> 13, 14 or 15
glass = pal.add((180, 210, 255), SHINY)   # -> 8..12
pal.set(16, (0, 0, 0))                    # deliberate hollow marker
```

Ordinary colours are allocated from **22 upward**, deliberately skipping 0–21
so the same generator is safe for humanoid and non-humanoid art alike. There
are only **3 glowy slots and 5 shiny slots** in the whole palette — budget
them.

When extending an existing asset, `read_vox` returns a palette that has
already reserved every index the asset uses, so `add` cannot repaint a colour
the model depends on.
