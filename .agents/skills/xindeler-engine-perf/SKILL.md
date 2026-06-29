---
name: xindeler-engine-perf
description: Use when profiling, optimizing, or reviewing performance/memory-safety of engine code — tracy workflow, criterion benches, unsafe policy, hot-path catalogue
---

# xindeler-engine-perf

**REQUIRED:** Read `docs/design/specs/2026-06-10-engine-improvements-design.md`
before optimizing anything. Rule zero: **measure first** — no optimization PR without a
before/after measurement attached.

## Measurement toolbox (verified in this fork)

| Tool | How |
|---|---|
| Tracy (CPU) | Build with `--features tracy`; cargo aliases `tracy-server`/`tracy-server-debuginfo` in `.cargo/config.toml`; spans via `prof_span!()` |
| Tracy (heap) | `tracy-memory` feature (global allocator instrumentation) |
| GPU | `wgpu-profiler` integration (`voxygen/src/render/renderer/mod.rs:~169`) |
| ECS system timings | `SysMetrics`/`PhysicsMetrics` (`common/ecs/src/metrics.rs`) |
| Criterion benches | Exist in common (chonk/color/loot), voxygen (meshing), world (cave/site/tree), network — `cargo bench -p <crate>` |
| Telemetry | fork's `telemetry!` + JSONL sinks (`xindeler-telemetry` skill to analyze) |

## Hot-path catalogue (where perf work pays)

- Terrain meshing: `voxygen/src/mesh/terrain.rs` (`calc_light` serial BFS allocates two
  full-volume Vecs per chunk — known issue, see spec §A1), `voxygen/src/mesh/greedy.rs`.
- Physics: `common/systems/src/phys/mod.rs` — par_joins at L389/L724/L861, but spatial
  grid rebuilds are serial (L1477/L1480).
- Shader/pipeline recreation: `voxygen/src/render/renderer/pipeline_creation.rs` — already
  backgrounded; the issue is the fresh all-cores rayon pool starving the render thread.
- Telemetry layer: global `Mutex<BufWriter>` per event (spec §A4 ring-buffer fix).
- rtsim tick: budget-sensitive once AURORA/ORACLE land — watch via SysMetrics.

## Unsafe policy

- Real unsafe blocks ≈ 10 across 7 files (most grep hits are mechanical
  `unsafe(export_name)` attributes). 5 crates already `#![deny(unsafe_code)]`.
- New unsafe requires: a `// SAFETY:` comment stating the invariant, a test exercising the
  boundary, and sign-off via `rust-perf-reviewer` dispatch.
- `common/state/src/plugin/memory_manager.rs` is the highest-risk file — its refactor is
  specced (§B2); don't patch around it.

## Optimization workflow

1. Capture baseline (tracy capture or criterion bench) on a representative scene/save.
2. Write the bench if none covers the path — bench-first is the perf version of TDD.
3. Change one thing. Re-measure. Keep the numbers in the PR description.
4. `cargo clippy` (CI feature set, see the repository instructions) + dispatch `rust-perf-reviewer`.
5. Watch for upstream-merge surface: prefer new modules over rewriting upstream files.
