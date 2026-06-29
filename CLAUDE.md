# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Interaction Convention — Fill-in Worksheets (Matias ⇄ Claude)

Whenever you need Matias to **make decisions, choose between options, confirm renames/changes, or supply information**, do **not** scatter the questions through prose or rely only on `AskUserQuestion`. Instead present a **plain-text fill-in worksheet** Matias can copy into Sublime Text, complete offline, and paste back whole — easy for him to fill, unambiguous for you to parse, with tables that never break alignment.

Rules (full spec + canonical example: `docs/design/conventions/fill-in-worksheets.md`):
- Wrap the entire worksheet in a fenced code block so it renders monospace; align all columns and `->` arrows.
- Header box with `=====` borders stating what it is and what happens on confirm; sections numbered and split by `------` rules.
- **Bulk confirmations in a BLOCK** with one global `[DG] decisión global:` + `excepciones:` field ("OK a todos" once), and "(se mantienen / ya confirmados …)" notes so he sees what is NOT changing.
- **Real decisions as `[Q1]`, `[Q2]`, …**, each with a `decisión:` blank line; coinages get **OPCIÓN A / OPCIÓN B**, a `[pick]`, and a free `propio` column.
- Final action section `[P1] … (SI / NO)`; close with `FIN. Devolveme el bloque completado.`

This is the default for any multi-decision / bulk request (`AskUserQuestion` only for 1–4 quick structural forks).

## Toolchain

Nightly Rust is required (pinned in `rust-toolchain`). The project uses the 2024 edition. The `specs` ECS crate requires nightly.

## Commands

```bash
# Run the game client (hot-reloading enabled by default in dev builds)
cargo run --bin veloren-voxygen

# Run the server
cargo run --bin veloren-server-cli

# Tests require the assets path
VELOREN_ASSETS="$(pwd)/assets" cargo test

# Single crate test
VELOREN_ASSETS="$(pwd)/assets" cargo test -p veloren-common

# Lint (matches CI exactly)
cargo clippy --all-targets --locked \
  --features="bin_cmd_doc_gen,bin_compression,bin_csv,bin_graphviz,bin_bot,bin_asset_migrate,asset_tweak,bin,stat,cli" \
  -- -D warnings

# Clippy for voxygen publish profile (no hot-reloading)
cargo clippy -p veloren-voxygen --locked --no-default-features --features="default-publish" -- -D warnings

# Format check
cargo fmt --all -- --check

# Release build (no hot-reloading, with LTO)
cargo build --release --no-default-features --features default-publish
```

## Workspace Architecture

The project separates into four layers:

**Executables**
- `voxygen/` — GUI client. Owns rendering (wgpu), windowing (winit), UI (egui + conrod), audio, and asset hot-reloading. The `hot-reloading` feature (on by default in dev) loads animation and agent code as dynamic libraries via `common-dynlib`.
- `server-cli/` — Headless server binary wrapping the `server` crate.

**Game logic**
- `server/` — Authoritative game state: ECS tick, player connections, persistence, economy.
- `client/` — Client-side game logic and networking (no graphics).
- `server-agent/` — NPC AI behavior, compiled as a hot-reloadable dylib in dev.
- `rtsim/` — Long-running world simulation (NPC migrations, factions, civilization events).
- `world/` — Procedural world generation: terrain, sites (towns/dungeons), caves, trees.

**Common layer** (`common/` + sub-crates)
- `common/` — Core game types: components, items, recipes, combat formulas, terrain chunks.
- `common-state/` — ECS world setup; integrates plugins; shared between client and server.
- `common-systems/` — ECS systems (physics, buffs, projectiles, etc.) run on both sides.
- `common-net/` — Network message types and compression.
- `common-assets/` — Asset loading abstraction over the `assets_manager` crate.
- `common-ecs/` — ECS utility traits on top of `specs`.

**Network**
- `network/` — Low-level multiplayer transport (TCP, QUIC via Quinn, optional metrics).
- `network-protocol/` — Wire format and message serialization.

## ECS Pattern

The codebase uses `specs`. Components live in `common/src/comp/`, resources in `common/src/resources.rs`. Systems in `common-systems/` are registered in `common-state/`. Server-only systems are in `server/src/sys/`. Always check existing comp/system patterns before adding new ones.

## Assets

All game data (voxel models, audio, i18n strings, configs) lives in `assets/`. The build reads `VELOREN_ASSETS` at runtime; in dev it defaults to `$(pwd)/assets`. Asset configs use RON format. Items, recipes, and entity configs are data-driven and live under `assets/common/`.

The large **binary** assets are stored via Git LFS on a self-hosted VPS store, **not** on GitHub — see **Git LFS & Binary Assets (the VPS)** below.

## Hot-reloading

In dev builds, `voxygen-anim` and `server-agent` are compiled as `cdylib` crates and loaded at runtime. Changes to animation or AI code reload without restarting. This is gated by the `hot-reloading` feature; the `default-publish` feature set disables it for release builds.

## Features of Note

- `tracy` — Enables Tracy profiler integration across crates.
- `asset_tweak` — Allows runtime asset value tweaking for balancing.
- `simd` — Enables SIMD optimizations in server-cli.
- `bin_*` — Various utility binaries (CSV export, graph generation, bot, asset migration).

## Documentation & Git Policy

**Where docs live — two repos, one working tree:**
- Design docs (specs, plans, task boards) live in `docs/design/`, which is a **separate, private git repo** (`Matute289/xindeler-design`) nested inside this one and gitignored here. Commit and push design docs from inside `docs/design/` — never into this (public) repo.
  - Specs → `docs/design/specs/`, implementation plans → `docs/design/plans/`, task boards → `docs/design/tasks/` (index: `00-task-board.md`).
- Lore canon (markdown) lives at `docs/design/lore/` in the private design repo. `docs/lore/` is a legacy path kept gitignored as a guard — never create files there.
- `.superpowers/` (brainstorm scratch) and `graphify-out/` are local-only and gitignored; never commit them anywhere. Brainstorm conclusions belong as a spec/plan in `docs/design/`.
- The `gitlab` remote is the fetch-only upstream (push disabled); never push to it.

**Branch protection (public repo `Matute289/xindeler`):**
- `main` and `development` are protected: no direct pushes (admins included), no force-pushes, no deletion. All changes land via PR with 1 approval.
- AI agents must NEVER merge or approve PRs, push to `main`/`development`, or touch branch-protection settings. Workflow: branch off `development` → commit → push branch → open PR with base `development` → stop and report. Only Matias reviews and merges.

## Git LFS & Binary Assets (the VPS) — IMPORTANT

Large binary assets (`.vox`, `.png`/`.jpg`/`.jpeg`, `.ogg`/`.wav`, `.ttf`, `.ico`, `.obj`/`.blend`, `assets/world/map/*.bin`, etc. — the full list is `.gitattributes`) are **NOT stored on GitHub**. They live on a self-hosted Git LFS store on the VPS. GitHub holds only code, RON/i18n text, and tiny **LFS pointer files**.

**Topology — three sources, one working tree:**
- **GitHub public** (`Matute289/xindeler`, `origin`) — code + RON/i18n + LFS pointers. No blobs.
- **VPS** (`greenmountain.dev:/srv/git-lfs/repos/xindeler.git`) — the actual binary blobs, served by `git-lfs-transfer` over **pure SSH** (no HTTP server, no Caddy). Private (SSH-key auth). It is the **single copy** of the binaries, so it must be backed up server-side. Server-side setup notes live in the private `MyServerVPS` repo (`git-lfs/`).
- **GitHub private** (`Matute289/xindeler-design`, nested at `docs/design/`) — design/lore.

**How it's wired:**
- `.lfsconfig` (committed) sets `lfs.url = ssh://mgrinberg@greenmountain.dev/srv/git-lfs/repos/xindeler.git`. Every clone reads it, so all LFS push/fetch goes to the VPS — never GitHub.
- `.gitattributes` tracks **only binaries**. RON/i18n and all text stay as normal git files — data-driven content travels with the code; never LFS-track it.
- Requires **git-lfs ≥ 3.0** on every client (there is no HTTP fallback) plus SSH access to the VPS to fetch/push blobs.

**Rules going forward:**
- **Never re-introduce GitHub LFS.** No workflow may `actions/checkout` with `lfs: true` against GitHub, nor `git lfs push … github`. Route LFS to the VPS: local work uses the committed `.lfsconfig`; CI must add a `Setup SSH` step with `secrets.VPS_SSH_KEY` and pull from the VPS (see `publish-docker.yml` for the pattern).
- To add new binary assets, just commit them normally — the pre-push hook sends blobs to the VPS automatically; GitHub gets only the pointer.
- Without VPS SSH access, a clone gets code + pointers but **not** the real binaries — this is the intended privacy boundary (assets stay private).

## Releases & CI

**Where each build runs:**
- **Code CI** (build / check / test / lint on PRs) → **GitHub Actions** (public repo = free, unlimited minutes). It must **not** pull LFS — compilation and tests don't need the binary assets.
- **Server release** → built **on the VPS** (where the assets are local), not on GitHub Actions. `release.yml` triggers on a `v*` tag push, SSHes to the VPS with `secrets.VPS_SSH_KEY`, and runs `/srv/git-lfs/scripts/build-release.sh <tag>` → produces `/srv/git-lfs/releases/xindeler-server-<tag>.tar.gz`.
- **Docker image** (`publish-docker.yml`, manual) → pulls only the asset dirs the image bundles (`assets/common,server,world`) from the VPS, builds `veloren-server-cli`, pushes to GHCR.
- **Client release** (voxygen desktop installer + Airshipper) → **deferred** to the first client release; study Veloren's packaging then. The shipped client necessarily bundles its assets (players have them locally) — "private" means private in source control, not in the shipped binary.

**GitHub Actions minutes:** the 2,000-minute quota is for **private** repos only; the public `xindeler` repo runs Actions for free. Heavy Rust builds run on the VPS anyway, so they don't consume GitHub minutes.

## Upstream Sync (GitLab Veloren)

Xindeler is a fork of `gitlab veloren/veloren` (the `gitlab` remote — fetch-only, never push). To pull upstream `master` and update without breaking or overwriting Xindeler's work:

- **Use the `gitlab-master-merger` skill** together with the `upstream-sync.yml` workflow. They bring upstream changes into a **review branch** (`upstream/review-…`) and integrate via **PR** — they do **not** force-push `main`/`development`.
- ⚠️ **Never hard-mirror** upstream over our branches. (The old `mirror.yml` did `git push --force master→main` and was removed for exactly this reason; branch protection blocks it anyway.)
- Upstream brings its own LFS binaries — these route to the **VPS** via `.lfsconfig`, never to GitHub.
- After a sync, run the lint/test commands above and resolve conflicts so Xindeler customizations (classes, races, magic, lore-driven assets, CI/LFS config, etc.) are preserved — upstream must never clobber them.

## Build Profiles

Custom profiles in the workspace `Cargo.toml`:
- `dev` (default): opt-level=2, debug assertions on — faster iteration than a true debug build.
- `release`: opt-level=3, full LTO, `panic=abort`.
- `no_overflow`: Used in world-gen crates to skip overflow checks for performance.

## 📋 Project Backlog (scored & prioritized)

**The full scored master backlog lives in [`docs/backlog/backlog.md`](docs/backlog/backlog.md)** — moved
out of this file to keep CLAUDE.md lean. It is **the single always-present roadmap** (all `BL-NN` epics +
the scoring rubric + dependency edges + parallel tracks), each row referencing its specs/plans/tasks in
the private `docs/design/`.

**Always read `docs/backlog/backlog.md` on resume and before starting / after finishing any work**, together
with `docs/design/session-notes.md` + `agenda.md`. The backlog is **multi-session**: `git pull`/re-sync
`development` before editing, add+score new `BL-NN` rows **there** (not here), and commit only your own rows.

