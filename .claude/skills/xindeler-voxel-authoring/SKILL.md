---
name: xindeler-voxel-authoring
description: Use when building a `.vox` asset by writing code that places voxels — creatures, monsters, weapons, props, and spell-effect objects — rather than sculpting in MagicaVoxel or generating one with an AI service, and when wiring such an asset into the engine so a skeleton can animate it. Covers the exact `.vox` subset the engine reads, the load-bearing palette-index conventions, how segments map to skeleton bones, which spell VFX actually suit voxel geometry versus particles, and the full asset→manifest→Body wiring. Not for terrain masks (use cromatolis-cartography) and not for AI-service model generation (that is the user-level `meshy` / `voxelai` skills).
---

# xindeler-voxel-authoring

Voxel assets in this engine are **data, and small enough to compute**. A wolf
head is 437 voxels. A cannon barrel is 14,825. That means anything you can
describe as a formula — a sphere, a ring, a mirrored pair of limbs, forty
recolours of the same sword, a rune circle whose spoke count comes from the
spell's level — is better *generated* than sculpted, and a `.vox` file is a
format you can write directly from a 300-line Python module with no
dependencies and no editor in the loop.

This skill is the **code-authored** path. The sibling paths, for contrast:

| You want | Use |
|---|---|
| Precise geometry, symmetry, parametric variants, palette-exact recolours, batch-generating families of assets, editing an existing `.vox` voxel-by-voxel | **this skill** + the `voxel-asset-engineer` agent |
| An organic creature you'd rather describe in words than in code | the user-level `meshy` skill (Meshy AI → GLB → `.vox`) or `voxelai` |
| Terrain heightmaps/masks for Cromatolis | `cromatolis-cartography` |
| Where a new body/ability/asset *belongs* in the repo | `game-architecture` |
| A turntable/flythrough **render** of a model for key art or marketing | MagicaVoxel + `MagicaVoxel-Animation-Script`, filed with `xindeler-graphic-assets` — **not** a game-asset tool, see `references/06` |

## The big idea, in four facts

Everything in the references follows from these. They were verified by
running code against the real engine, not read off a format spec.

1. **The engine reads a tiny subset of `.vox`.** `Segment::from_vox`
   (`common/src/figure/mod.rs:65`) reads `models[model_index]` and `palette`.
   It ignores the scene graph, layers and materials entirely — no *engine*
   consumer reads `.scenes`, `.layers`, `.materials` or `.index_map`. Every
   shipped asset sampled has `scenes = 0, layers = 0, materials = 0`
   (the exception, `char_template.vox`, is an authoring reference that no code
   loads — and whose scene graph turns out to be the only record of which
   model is which body part, see `references/06`). So a valid
   asset is just `MAIN { SIZE + XYZI …, RGBA }` — and you never need
   `nTRN`/`nGRP`/`nSHP` to make a segmented, animatable figure.

2. **The palette index decides the material, not the colour.**
   `Cell::from_index` (`common/src/figure/cell.rs:100`) makes indices 8–12
   shiny, 13–15 self-illuminating, 16 a *carving* voxel that deletes other
   segments, and 17–21 carve-proof. Put a voxel on the wrong index and it
   silently glows, goes translucent, or vanishes. This is the single most
   consequential fact in the whole pipeline and it is documented nowhere in
   the engine source. → `references/02-palette-and-materials.md`

3. **Segmentation lives in the manifest, not in the file.** A creature is
   *N separate `.vox` models* (either N files or one file with N models
   picked by `model_index`), each named in a RON manifest, each with an
   `offset`, and each assigned to a skeleton bone **by position in a 16-slot
   array**. That mapping used to be checked by nothing at all; the *bone-name
   order* a body is written against is now compiler-checked, via
   `make_vox_spec!`'s `bones:` clause against the skeleton's
   `MESH_BONE_NAMES`. Which expression you put in which slot is still
   convention. → `references/03-figures-bones-and-skeletons.md`

4. **There is no keyframe animation in `.vox` anywhere in this engine.**
   "Animating a voxel model" means writing Rust that transforms rigid
   segments per frame. Matías's framing — *start from existing skeletons and
   attach our own `.vox` skins* — is **true for reskinning an existing
   species** (genuinely zero Rust) and **false for a new species** (zero new
   animation *functions*, but ~16 mandatory match arms of tuning numbers, plus
   a handful more that silently *default* rather than failing to compile —
   those are now caught by a fall-through audit, see `references/03`).
   → `references/03` has the honest breakdown. Baked multi-frame animation
   was investigated against real prior art and **does not fit**: `model_index`
   is fixed at mesh-build time and the mesh cache has no time dimension.
   → `references/06`.

## Read first

Read the reference that covers your task; they are short and each one is
self-contained. They live beside this file, in the repo — not in
`docs/design/` — because they describe public engine code and a worktree
session must be able to read them without a private-repo pull.

- `references/01-vox-format-and-engine-loader.md` — the byte format, what the
  engine reads and skips, the hard limits (256/axis, 512³ segment, ±128
  offset, index 255 is a crash), verified edge cases.
- `references/02-palette-and-materials.md` — **read before placing a single
  voxel.** Index→material table, the humanoid `MatSegment` trap, the
  hollow/override-hollow layering mechanism, `custom_indices` for Fire /
  Water / SwirlyCrystal.
- `references/03-figures-bones-and-skeletons.md` — bones, offsets and pivots,
  central/lateral manifests, `model_index` packing, what "reuse an existing
  skeleton" really costs, per-body-kind bone lists, the **two safety nets**
  (compile-time bone-order check, and the attribute fall-through audit), and
  the standing proposal for fully named mesh slots.
- `references/04-spell-vfx-what-fits-voxels.md` — the six VFX surfaces the
  engine has, and a blunt verdict on which spell effects should be authored
  `.vox` and which must stay particles or shaders.
- `references/05-authoring-workflow.md` — the actual process: `voxlib.py`,
  shape primitives, symmetry, the verify loop, where files go, manifest
  wiring, Git LFS.
- `references/06-magicavoxel-interop.md` — importing a MagicaVoxel-authored
  file: reading the scene graph to recover which `model_index` is which part,
  `_t` vs manifest `offset`, and the verdict on MagicaVoxel's animation
  features and the `MagicaVoxel-Animation-Script` tool (a camera/lighting
  render robot — it does not apply to game assets).

The authoring library itself is `tools/voxel/voxlib.py` (no third-party
dependencies, Python 3.9+). `python3 tools/voxel/selftest.py` proves it still
round-trips.

> **Known doc conflict, not yet resolved:** the repo `CLAUDE.md`'s macOS run
> command drops the `hot-reloading` feature, citing the `common/dynlib` macOS
> failure — but `hot-reloading` is the *asset* watcher and never touches
> `common/dynlib`; only `hot-anim`/`hot-egui` do, and neither is in the default
> feature set. Reference 03 has the verified feature table. `CLAUDE.md` should
> be corrected rather than have this skill quietly override it — raise it with
> Matías rather than assuming either document is authoritative.

## Workflow

1. **Decide it's a voxel problem at all.** For a spell effect, read
   `references/04` first — several of the effects people ask for (beams,
   explosions, lightning, poison clouds) are *worse* as voxel meshes than as
   the particles they already are.
2. **Pick the body kind** — `Body::Object` (2 bones, easiest, right for props
   and VFX), an existing NPC species reskin (free animation), or a new
   species (see the cost table in `references/03`).
3. **Write a generator script** under `tools/voxel/` or the scratchpad, using
   `voxlib`. Build each posable part as its own `VoxModel`. Keep the pivot at
   the local origin while you build — `normalised()` hands back the offset you
   owe the manifest.
4. **Lint and verify**: `voxlib.lint()` catches accidental glow/carve indices
   and oversized models; then load the file through the *real* engine reader
   (the throwaway-example recipe in `references/05`) and assert sizes, voxel
   counts and surfaces. Never ship a `.vox` you have only inspected with a
   viewer — every `.vox` mistake in this engine is silent.
5. **Wire it**: asset under `assets/voxygen/voxel/<category>/`, a row in the
   matching `*_manifest.ron`, and the `Body` variant if it's new.
6. **Ship it**: `.vox` is Git-LFS-tracked to the VPS (see the repo CLAUDE.md);
   just commit normally, the pre-push hook routes the blob. Branch off the
   current working branch, one PR, never merge.

## Non-negotiables

- **Verify against the engine's own loader, not a viewer.** A voxel whose
  palette index has no colour is *dropped silently*; a model index past the
  end returns a *zero-sized segment* silently. (A bone *name list* that drifts
  out of sync with its skeleton is now a compile error — see `references/03` —
  but nothing else on this list is, and putting the right name on the wrong
  array slot still isn't.)
- **Run the audits after adding a species.**
  `cargo test -p xindeler-common -p xindeler-anim --lib attr_audit` (the same
  command CI runs) names any *audited* attribute you left to a silent default —
  `mass`, `base_health`, `base_poise`, `base_energy`, `threat_tier`, `scaler`,
  `tempo` and friends. It is a real gate, not a complete one: `references/03`
  lists exactly what is in and out of scope. Fix the species; re-bless the
  ledger only when the catch-all genuinely is right for it.
- **Never use palette index 255.** `dot_vox` serialises index `i` as `i + 1`;
  255 overflows a `u8` and panics *its* writer (`voxlib` rejects it up front
  with a clear error instead). Verified by running it.
- **Pass the right `kind` to `lint()`.** The sprite mesher asserts 32×32×64 and
  the particle mesher 16×16×64 — hard panics, far below the 512³ figure cap.
- **Never use indices 0–7 on a `Body::Humanoid` asset.** They are the
  skin/hair/eye recolour channels and will be repainted at runtime.
- **Reserve before you extend.** When adding colours to an existing asset's
  palette, `read_vox` already reserves every index the asset uses — don't
  hand-pick a slot without checking.
- **`.vox` is LFS.** Never add a workflow that pulls LFS from GitHub; blobs
  live on the VPS.
