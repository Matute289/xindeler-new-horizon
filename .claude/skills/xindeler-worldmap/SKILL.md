---
name: xindeler-worldmap
description: Use when authoring or editing Xindeler's hand-designed world maps — the Highlands continent (terrain, coastline, rivers, biomes, swamps, pinned cities/towns, caves) and, later, the plane maps. Covers the lore→heightmap→.bin→import→site-pin→biome/cave→map-asset pipeline. For generic procedural worldgen internals use xindeler-worldgen instead.
---

# xindeler-worldmap

Authoring Xindeler's **designed** maps on top of Veloren's procedural worldgen. The design lives in
`docs/design/specs/2026-06-24-xindeler-worldmap-design.md` (+ plan + `tasks/21`). For the procedural
pipeline internals, pair with the **xindeler-worldgen** skill. Delegate map layout/cartography to the
**`worldmap-cartographer`** agent.

## The big idea
Veloren is procedural from a seed, **but it can IMPORT a heightmap** — and the heightmap is the single
source of the world's *shape*. Everything downstream (rivers, lakes, biomes, site candidates, caves) is
derived deterministically from altitude. So we **author terrain via a heightmap**, **pin** the canon
sites, **bias** biomes per region, **tune** caves, and **regenerate** the map asset. V1 = **Highlands
only**; planes are *planned* (not built) for V1.

## The pipeline (in order)
```
lore leaf (docs/design/lore/80-geography/*)   ← source of truth (borders, realms, cities, rivers)
      ↓  (worldmap-cartographer agent)
map-design doc (outline, mountains, rivers, region borders, site-pin coords, biome/swamp zones, caves)
      ↓  (heightmap tool: PNG + masks → WorldMap_0_7_0 .bin)
assets/world/map/<name>.bin   ← authored terrain (binary → VPS-LFS, never GitHub)
      ↓  FileOpts::LoadAsset
WorldSim loads heightmap → erosion/rivers/lakes/biomes derive from it
      ↓  authored-site-pin config (additive, in world/src/civ/)
canon cities/towns pinned at canon coords; civ fills the rest
      ↓  regional biome/forest/swamp bias + cave density tuning
      ↓
in-game world-map asset regenerated (markers + canon names) — world/src/lib.rs get_map_data, voxygen/hud/map.rs
```

## Key code levers (verified — see the BL-49 relevamiento in the spec)
- **Heightmap import:** `world/src/sim/mod.rs` `FileOpts::{Load, LoadAsset, LoadOrGenerate, Save}` +
  `WorldMap_0_7_0` (`alt`/`basement` arrays). Default maps: `assets/world/map/*.bin`.
- **World size:** `world/src/sim/mod.rs:88` (`DEFAULT_WORLD_CHUNKS_LG` 2^10) / `GenOpts::x_lg,y_lg`.
- **Biomes/forests:** `world/src/sim/mod.rs:2793` `BiomeKind` (Swamp is commented out — re-enable),
  `world/src/all.rs` 17 `ForestKind` (climate ranges) — biasable by region.
- **Sites:** `world/src/site/mod.rs` `SiteKind`, placement in `world/src/civ/mod.rs:229` (proximity;
  **no pin system yet** — add an additive `AuthoredSitePin`).
- **Caves:** `world/src/layer/cave.rs` (`LAYERS`, `CELL_SIZE`, spawn rates; biomes barren/mushroom/fire/
  leafy/dusty/icy/snowy/crystal/sandy).
- **Map asset/markers:** `world/src/lib.rs:173` `get_map_data`, `voxygen/src/hud/map.rs`.
- **Persistence:** authored `.bin` + derived features + rtsim persist; a designed world is stable.

## Canon to honour (Highlands, V1)
`docs/design/lore/80-geography/the-highlands.md` + `00-world-map.md`: central-east continent, river/
mountain-veined; **Merovingia** (NW, cold), **Cromatolis** (W/SW), **Xandrian** (E, Platinum City, Dark
Lands), **the Freelands** (centre, scattered free towns/ruins); **Aurora**/Dawn City NW; **Azuria
Ocean** W, **Abyssal Ocean** E, **Ventanor** N pole. Sub-region maps (✔) + sketch `~/MyXindeler/Mapas/
boceto.jpg`.

## Rules
1. **Additive + config-gated** in `world/` — it's upstream-owned and churns; never rewrite, extend.
   Document every `world/` edit for the next `gitlab-master-merger` sync.
2. **Binary map assets → VPS-LFS**, never GitHub (see the repository LFS policy).
3. **Iterate**: erosion reshapes an imported heightmap — expect several author→verify passes (fly the
   area, `/tp`, or the airship). Use `xindeler-run` to launch; capture logs.
4. **Lore is the source of truth** — geography leaves in `docs/design/lore/80-geography/`. New
   geography facts get a lore leaf first (`xindeler-lore`), then the map.
5. **V1 = Highlands only.** Other continents + plane maps + the plane-travel engine are V2+.

## Workflow
1. `worldmap-cartographer` → map-design doc from the lore + reference images.
2. Heightmap tool → `.bin`; load via `FileOpts::LoadAsset`; `cargo server` + connect; verify; iterate.
3. Add/extend the `AuthoredSitePin` config; pin canon sites; check markers.
4. Bias biomes/forests/swamps; tune caves; re-verify.
5. Regenerate the map asset; confirm markers + names.
6. Reviews: `game-architecture-reviewer`, `rust-perf-reviewer`, `xindeler-review`. Smoke = BL-09.
