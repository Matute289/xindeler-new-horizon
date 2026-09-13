---
name: cromatolis-cartography
description: Use when reviewing, diagnosing, or editing Cromatolis's authored terrain masks (heightmap, water, elevated lakes, biomes/vegetation) at the pixel/data level — not lore-driven map design (use xindeler-worldmap for that), and not generic procedural worldgen internals (use xindeler-worldgen for that). Covers the real xindeler-open-world → xindeler-new-horizon mask pipeline, coordinate math, the engine's authored-vs-procedural override bugs, and the verify-against-the-real-engine discipline this area requires.
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
- `01-coordinate-systems.md` — pixel↔chunk conversion formulas, the pixel-space-vs-chunk-space anisotropy trap.
- `02-elevation-and-heightmap.md` — L16 encoding, **the sea-level-offset gotcha** (read this before editing any altitude value), height bands.
- `03-hydrology-water-and-lakes.md` — water/elevated-lake mask semantics, `fill_sinks`, the full recipe for carving a valid closed basin.
- `04-biomes-vegetation-and-climate.md` — vegetation density → `tree_density` formula (a power curve, not linear), the temperature curve, altitude↔temp cheat sheet.
- `05-terrain-steepness-cliffs-and-flatness.md` — gradient/cliff-detection thresholds, safe slope design numbers.
- `06-site-placement-predicates.md` — what each dungeon/site type actually requires, and the authored-landmark bypass that makes most predicate edits no-ops for Cromatolis.
- `07-authored-vs-procedural-priority.md` — the recurring "authored data silently overridden by an unrelated procedural pass" bug class, and how to find/fix another instance.
- `08-tooling-and-verification-workflow.md` — the exact commands, the throwaway-sampler pattern, and the non-negotiable verify-against-the-real-engine discipline.

These docs are living references — update them in place (as their own
small PRs to `docs/design`) whenever you learn something that contradicts
or extends them, per Matías's explicit ask to keep each topic separately
editable rather than one enormous doc.

## When to use this skill vs. a sibling

| Task | Use |
|---|---|
| Turning region/plane lore into a map-design doc (no pixel editing) | `xindeler-worldmap` skill + `worldmap-cartographer` agent |
| Generic procedural worldgen internals unrelated to Cromatolis's authored masks | `xindeler-worldgen` skill |
| Reviewing/diagnosing/editing Cromatolis's actual mask pixels, or a placement-predicate bug that might be a mask problem or an engine bug | **this skill**, delegate to the `cromatolis-terrain-engineer` agent |

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
3. Cross-repo changes (mask edit in `xindeler-open-world`, regenerated
   assets + any test re-baselining in `xindeler-new-horizon`, backlog
   status in `docs/design`) go through this repo's standing review-then-PR
   workflow — see `08-tooling-and-verification-workflow.md`'s "Cross-repo PR
   discipline" section.
4. If the agent finds a real engine bug (an authored value getting
   silently overridden — see `07`), fix it in the engine, not by
   contorting the mask to route around it — matching this project's
   established "fix the bug, don't work around it in the map" precedent.
