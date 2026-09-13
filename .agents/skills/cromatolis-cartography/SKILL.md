---
name: cromatolis-cartography
description: Use when reviewing, diagnosing, or editing Cromatolis's authored terrain masks (heightmap, water, elevated lakes, biomes/vegetation, routes, caves) at the pixel/data level, or when diagnosing why a site/dungeon/feature does or doesn't appear in Cromatolis — not lore-driven map design (use xindeler-worldmap for that), and not generic procedural worldgen internals (use xindeler-worldgen for that). Covers the real xindeler-open-world → xindeler-new-horizon mask pipeline, coordinate math, the engine's authored-vs-procedural override bugs, and the verify-against-the-real-engine discipline this area requires.
---

# cromatolis-cartography

Cromatolis is a hand-authored region running on an engine built for
procedural worlds. Reviewing or editing its terrain masks is a **numerical
data problem**, not a visual-art-review problem: the master masks are
32768×24576 16-bit TIFFs, far beyond any vision model's pixel-exact
precision, and several real bugs this area has produced were only
detectable by sampling raw array values and the real generated
`SimChunk` fields — never by "looking at" an image and judging it correct.

## Read first, every time

The full reference set lives in
`docs/design/references/cromatolis-cartography/` (pull `docs/design` first
— see this repo's standing rule). Read `00-overview-and-pipeline.md` first
for the big picture, then whichever topic doc is relevant to the task:

- `00-overview-and-pipeline.md` — the three repos, the full mask→engine chain, which mask feeds which engine field.
- `01-coordinate-systems.md` — pixel↔chunk conversion formulas, the pixel-space-vs-chunk-space anisotropy trap, and why single-pixel edits don't land.
- `02-elevation-and-heightmap.md` — L16 encoding, **the sea-level-offset gotcha** (read this before editing any altitude value), height bands.
- `03-hydrology-water-and-lakes.md` — water/elevated-lake mask semantics, `fill_sinks`, the full recipe for carving a valid closed basin.
- `04-biomes-vegetation-and-climate.md` — vegetation → `tree_density` (an altitude factor *then* a power curve), the temperature curve, altitude↔temp cheat sheet.
- `05-terrain-steepness-cliffs-and-flatness.md` — gradient/cliff-detection thresholds, safe slope design numbers.
- `06-site-placement-predicates.md` — what each dungeon/site type actually requires, and the authored-landmark bypass that makes most predicate edits no-ops for Cromatolis.
- `07-authored-vs-procedural-priority.md` — the recurring "authored data silently overridden by an unrelated procedural pass" bug class, plus an open candidate list.
- `08-tooling-and-verification-workflow.md` — the exact commands, the throwaway-sampler pattern, and the non-negotiable verify-against-the-real-engine discipline.
- `09-routes-bridges-and-caves.md` — routes and caves (live, with real mechanics), bridges (env-gated off), and which masks are genuinely inert.

⚠️ **These docs were corrected in a second verification pass on
2026-09-13.** Several formulas in the first draft were wrong. Anything you
remember from an earlier session or transcript should be re-read rather than
reused — corrections are marked ⚠️ inline.

These are living references — update them in place (as their own small PRs
to `docs/design`) whenever you learn something that contradicts or extends
them, per Matías's explicit ask to keep each topic separately editable
rather than one enormous doc.

## When to use this skill vs. a sibling

The boundary is **which region and which pipeline**, not which subject.
Cromatolis and the Highlands both "author a map with a heightmap and masks",
which makes them easy to confuse — but they are two entirely different
pipelines that happen to both end in `assets/world/map/`.

| Task | Use |
|---|---|
| Anything touching `cromatolis_v0*` assets, the L16 TIFF masters in `~/MyXindeler/OpenWorld/Cromatolis/l16-v10/`, or the `xindeler-open-world` repo | **this skill**, delegate to the `cromatolis-terrain-engineer` agent |
| "Why does/doesn't `<site, dungeon, cave, road, lake>` appear **in Cromatolis**?" — even if you suspect an engine bug rather than a mask problem | **this skill** (`06` + `07` exist precisely for this; most such reports are misdiagnosed) |
| The **Highlands** continent — `WorldMap_0_7_0` `.bin` authoring, its own heightmap+mask tool, site pinning | `xindeler-worldmap` skill + `worldmap-cartographer` agent |
| Turning region/plane **lore** into a map-design doc (no pixel editing, any region) | `xindeler-worldmap` skill + `worldmap-cartographer` agent |
| Procedural worldgen internals with **no authored region involved** — erosion, biome rules, `cave.rs`, site plot geometry, rtsim civ behaviour | `xindeler-worldgen` skill |

**Overlap cases, resolved:**

- *An engine fix that affects both Cromatolis and procedural worlds* (the
  `07` bug class): start here to diagnose and scope it, because the evidence
  is Cromatolis-shaped, but the fix is ordinary engine work — hold it to
  `xindeler-worldgen`'s and this repo's normal review bar.
- *Cave/route/bridge questions*: read `09` first. It is a common trap that
  these look like raster-mask work and are actually RON/point-list work, or
  gated off entirely by an env var.
- *"There are no automated tests for world-gen output"* — true for
  `xindeler-worldgen` generally, **false here**. Cromatolis has an
  `#[ignore]`d placement-coverage regression test and 17 other ignored
  real-data tests. Don't inherit the sibling skill's assumption.

## Workflow

1. Delegate the actual work to the **`cromatolis-terrain-engineer`** agent
   — it has `Bash` access for the numpy/PIL scripts and `cargo` rebuilds
   this work requires; a plain review-only agent can't verify anything
   here.
2. That agent will: read the relevant topic docs above, inspect the real
   pixel/array data (never "look at" the master TIFF visually), make any
   edit as a new `_v<N+1>.tif` revision (never overwriting an existing
   file), rebuild the full pipeline, and verify against the real regenerated
   engine before reporting anything as fixed.
3. **The agent cannot dispatch subagents.** If its work touches engine code,
   *you* run the specialist reviewers (`ecs-design-reviewer`,
   `game-architecture-reviewer`, `rust-perf-reviewer`) against the real diff
   and fix findings **before** anything is pushed — this repo never opens a
   PR and then fixes it with follow-up commits.
4. Cross-repo changes (mask edit in `xindeler-open-world`, regenerated
   assets + any test re-baselining in `xindeler-new-horizon`, backlog
   status in `docs/design`) go through this repo's standing review-then-PR
   workflow — see `08`'s "Cross-repo PR discipline" section. One PR per
   repo, cross-linked, and never merged by an agent.
5. If the agent finds a real engine bug (an authored value getting
   silently overridden — see `07`), fix it in the engine, not by
   contorting the mask to route around it — matching this project's
   established "fix the bug, don't work around it in the map" precedent.
   ⚠️ Note that fixes in this area often move the placement-coverage
   regression test's counts, so budget for a real re-baseline rather than
   assuming a compile-and-ship.
