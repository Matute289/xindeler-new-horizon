# AGENTS.md

This file provides guidance to Codex when working with code in this repository.

## What this repo is

`xindeler-new-horizon` is the successor track chosen 2026-07-24 after an engine-strategy
investigation (`docs/design/specs/2026-07-24-engine-strategy-veloren-vs-bevy.md`, private repo)
concluded that reverting to the original Veloren-derived engine — rather than continuing the
from-scratch Bevy 0.19 port — was the more viable path. Three related repos exist:
- **`xindeler-new-horizon`** (this repo) — the live, ongoing successor project. All new work
  happens here.
- **`xindeler-old`** (sibling local checkout) — the frozen source this repo was cloned from
  (Veloren-derived engine, still `veloren`-branded). Kept as a clean reference; not touched by
  new-horizon work.
- **`xindeler`** (sibling local checkout) — the earlier Bevy 0.19 port. Superseded, not deleted;
  its shared sim-crate history and any future frozen-reserve decision are documented in the
  engine-strategy spec above.

Full rationale, the conditional pivot plan, and the `NH-N` backlog rows all live in
`docs/design/` (the same private repo nested in `xindeler`, reused here — see below).

## Interaction Convention — Fill-in Worksheets (Matias ⇄ Codex)

Whenever you need Matias to **make decisions, choose between options, confirm renames/changes, or supply information**, do **not** scatter the questions through prose or rely only on `AskUserQuestion`. Instead present a **plain-text fill-in worksheet** Matias can copy into Sublime Text, complete offline, and paste back whole — easy for him to fill, unambiguous for you to parse, with tables that never break alignment.

Rules (full spec + canonical example: `docs/design/conventions/fill-in-worksheets.md`):
- Wrap the entire worksheet in a fenced code block so it renders monospace; align all columns and `->` arrows.
- Header box with `=====` borders stating what it is and what happens on confirm; sections numbered and split by `------` rules.
- **Bulk confirmations in a BLOCK** with one global `[DG] decisión global:` + `excepciones:` field ("OK a todos" once), and "(se mantienen / ya confirmados …)" notes so he sees what is NOT changing.
- **Real decisions as `[Q1]`, `[Q2]`, …**, each with a `decisión:` blank line; coinages get **OPCIÓN A / OPCIÓN B**, a `[pick]`, and a free `propio` column.
- Final action section `[P1] … (SI / NO)`; close with `FIN. Devolveme el bloque completado.`

This is the default for any multi-decision / bulk request (`AskUserQuestion` only for 1–4 quick structural forks).

**Sibling convention — in-game smoke-test checklists:** when what you need from Matias is not a
decision but a **manual test/QA pass** (build it, run the client, check boxes for pass/fail), use
the different shape documented in the same file's "Sibling convention" section — plain Markdown
(not one big fenced block), a `## Cómo correrlo / Setup` section with the exact build/run
commands, lettered `## A. <topic>` sections of `- [ ] **A1 · name** — ...` checkboxes, and a
closing `## Reportá así` legend (`✅`/`❌`/`🤷`, screenshots for failures, partial completion OK).
For a single small feature/PR, the lighter variant embeds this directly in that feature's
task-board doc instead of a standalone file.

## Delegation — worktrees + parallel subagents (Matias, 2026-07-24)

Ported from how `xindeler` (the sibling Bevy-port repo) actually worked in practice (never
formally written there either — this is the first time it's documented): for any task large
enough to split into independent pieces, don't implement everything serially yourself. Instead:

- **You are the orchestrator, verifier, and reviewer** — stay free to keep talking with Matias
  while subagents work in the background. Author each subagent's brief yourself: exact files,
  exact desired end-state, referencing the relevant plan/spec section — never "figure out task
  N4-C" with no other context.
- **Dispatch implementation subagents into isolated git worktrees** whenever more than one
  subagent could touch overlapping files concurrently, or a feature needs isolation from the
  current workspace. Skip the worktree only for a single, clearly-scoped, no-conflict-risk piece.
- **Run the existing specialist reviewer subagents** (`ecs-design-reviewer`,
  `game-architecture-reviewer`, `game-balance-designer`, `rust-perf-reviewer`,
  `sim-design-reviewer`, etc. — see `.codex/agents/`) against each diff before it ships, not just
  `cargo clippy`.
- **One PR per phase/task, off freshly-synced `development`.** You own git and the PR; you never
  merge (branch-protection rule below still applies to every subagent-produced commit too).
- For genuinely large or ambiguous new scope (a new subsystem, a cross-cutting content pass), have
  an agent investigate and write the plan + task board first (same pattern already used for
  NH-4/NH-19) — implementation dispatch only starts once that plan exists.

This is the default approach for multi-part work going forward, not just a one-off request.

## Comunicación asíncrona con Mati

Si necesitás input y Mati no está disponible (no está en la app, o el mensaje puede tardar):

```bash
MSG_ID=$(python /Users/mgrinberg/MyXindeler/Discord/scripts/discord_api.py notify \
  --project "xindeler-new-horizon" \
  --session "descripción corta de esta tarea" \
  --type blocked \
  --message "Qué necesitás decidir y por qué estás bloqueado")

# Polling hasta que responda (cada ~5 min)
python /Users/mgrinberg/MyXindeler/Discord/scripts/discord_api.py poll --after $MSG_ID
```

Tipos: `blocked` | `question` | `done` | `info` | `error`. Usalo también para avisar cuando una PR
queda lista para mergear, no solo cuando estás bloqueado — Mati no siempre está mirando el chat.

## Toolchain

Nightly Rust is required (pinned in `rust-toolchain`). The project uses the 2024 edition. The `specs` ECS crate requires nightly.

## Commands

```bash
# Run the game client (hot-reloading enabled by default in dev builds)
cargo run --bin xindeler-voxygen

# Run the server
cargo run --bin xindeler-server-cli

# Tests require the assets path
VELOREN_ASSETS="$(pwd)/assets" cargo test

# Single crate test
VELOREN_ASSETS="$(pwd)/assets" cargo test -p xindeler-common

# Lint (matches CI exactly)
cargo clippy --all-targets --locked \
  --features="bin_cmd_doc_gen,bin_compression,bin_csv,bin_graphviz,bin_bot,bin_asset_migrate,bin,stat,cli" \
  -- -D warnings

# Clippy for voxygen publish profile (no hot-reloading)
cargo clippy -p xindeler-voxygen --locked --no-default-features --features="default-publish" -- -D warnings

# Ensure xindeler-server-cli compiles standalone with the simd feature (matches CI)
cargo clippy --locked --bin xindeler-server-cli --no-default-features -F simd -- -D warnings

# Format check
cargo fmt --all -- --check

# Release build (no hot-reloading, with LTO)
cargo build --release --no-default-features --features default-publish
```

## Workspace Architecture

The project separates into four layers:

**Executables**
- `voxygen/` — GUI client. Owns rendering (wgpu), windowing (winit), the primary UI (conrod), audio, and asset hot-reloading. `voxygen/egui` is a separate debug/admin overlay (admin console, character-state inspector, shader experiments), not the main UI. The `hot-reloading` feature (on by default in dev) loads animation and agent code as dynamic libraries via `common/dynlib`.
- `server-cli/` — Headless server binary wrapping the `server` crate.

**Game logic**
- `server/` — Authoritative game state: ECS tick, player connections, persistence, economy.
- `client/` — Client-side game logic and networking (no graphics).
- `server/agent/` (crate `xindeler-server-agent`) — NPC AI behavior, compiled as a hot-reloadable dylib in dev.
- `rtsim/` — Long-running world simulation (NPC migrations, factions, civilization events, quests).
- `world/` — Procedural world generation: terrain, sites (towns/dungeons), caves, trees.

**Common layer** (`common/` + sub-crates — crate names are hyphenated, e.g. `xindeler-common-state`, but the directories are nested under `common/`, e.g. `common/state/`; there is no top-level `common-state/` directory)
- `common/` — Core game types: components, items, recipes, combat formulas, terrain chunks.
- `common/state/` — ECS world setup; integrates plugins; shared between client and server.
- `common/systems/` — ECS systems (physics, buffs, projectiles, etc.) run on both sides.
- `common/net/` — Network message types and compression.
- `common/assets/` — Asset loading abstraction over the `assets_manager` crate.
- `common/ecs/` — ECS utility traits on top of `specs`.
- `common/oracle/`, `common/query_server/` — small internal-subsystem/protocol crates; see the pointer below for what consumes `common/oracle/`.
- `common/base/`, `common/dynlib/`, `common/frontend/` — small foundational crates: shared macros/paths, the hot-reload dylib loader, and shared logging/telemetry setup, respectively.
- `common/i18n/`, `client/i18n/`, `voxygen/i18n-helpers/` — three-layer localization stack: `common/i18n` is the wire schema (`Content`/`LocalizationArg`, server → client), `client/i18n` (crate `i18n`) is the Fluent engine that resolves it (also home to the `i18n_check`/`i18n_csv` CLI tools), `voxygen/i18n-helpers` is display-layer glue for voxygen call sites.

**Network**
- `network/` — Low-level multiplayer transport (TCP, QUIC via Quinn, optional metrics).
- `network/protocol/` (crate `xindeler-network-protocol`) — Wire format and message serialization.

**PROJECT ORACLE / PROJECT AURORA** — internal subsystems; don't assume a module exists (or doesn't) from a grep alone. Code-location map moved to `docs/design/engine-notes/2026-08-21-oracle-aurora-implementation-map.md` (private) — read it before working on either.

## ECS Pattern

The codebase uses `specs`. Components live in `common/src/comp/`, resources in `common/src/resources.rs`. Systems in `common/systems/` are registered in `common/state/`. Server-only systems are in `server/src/sys/`. Always check existing comp/system patterns before adding new ones.

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
- Design docs (specs, plans, task boards) live in `docs/design/`, which is the **same separate, private git repo** used by the sibling Bevy-port project (`Matute289/xindeler-design`) — nested inside this repo too, and gitignored here. Commit and push design docs from inside `docs/design/` — never into this (public) repo. Reusing the same private repo (rather than forking a second one) keeps the engine-strategy decision history and the `docs/design/backlog/new-horizon.md` backlog in one place.
  - Specs → `docs/design/specs/`, implementation plans → `docs/design/plans/`, task boards → `docs/design/tasks/` (index: `00-task-board.md`).
  - The **working backlog for this project** is `docs/design/backlog/new-horizon.md` (private) — NH-N rows. It is now the **only** active backlog across all Xindeler repos: the sibling Bevy-port repo's own `docs/backlog/backlog.md` (`BL-NN`) and `docs/backlog/engine-migration.md` (`EM-N`) were merged in and retired 2026-09-05 — every unique `BL-N` row was ported here as `NH-94..NH-141`, and every duplicate/heritage row has a `🔁 BL-N` cross-reference note under the `NH-` row that covers it (search `🔁` to find any specific old `BL-N`). There is still no public `docs/backlog/backlog.md` in *this* repo — the working backlog stays private in `docs/design/`.
- Lore canon (markdown) lives at `docs/design/lore/` in the private design repo. `docs/lore/` is a legacy path kept gitignored as a guard — never create files there.
- `.superpowers/` (brainstorm scratch) and `graphify-out/` are local-only and gitignored; never commit them anywhere. Brainstorm conclusions belong as a spec/plan in `docs/design/`.
- The `gitlab` remote is the fetch-only upstream (push disabled); never push to it.

**Always `git pull` inside `docs/design/` immediately before reading OR writing anything there —
every time, not just at session start.** Matías works in `docs/design/` heavily and often
concurrently with agent sessions in this repo: another session may have pushed new commits, left a
branch with an open PR, or be mid-edit on an uncommitted change. Before reading a spec/plan/task
board to inform a decision, and before every `cd docs/design && git ...` write, run `git status` +
`git pull` first (on whatever branch you're on — check `git branch --show-current`, don't assume
`main`). If `git status` shows uncommitted changes or a branch that isn't yours, treat it as another
session's in-progress work per the general git-safety rules — do not discard, stash, or commit over
it; read around it or ask if it's genuinely blocking.

**Writes to `docs/design/` also go through a branch + PR, never a direct commit to `main` there**
(Matías, 2026-08-06) — the same discipline this repo already uses for code, applied to the nested
private repo too. `docs/design/` is a single shared working-tree checkout other sessions actively
read and write concurrently; committing straight to `main` risks colliding with another session
mid-checkout (observed directly: a concurrent session's branch switch mid-edit, twice in one
session). Concretely: `cd docs/design && git checkout main && git pull`, then
`git checkout -b <descriptive-branch-name>`, make the edit, commit, push, `gh pr create --base main
--head <branch>` (same repo, same `gh`, just pointed at `Matute289/xindeler-design`), then switch
back to `main` locally and stop — do not merge. If the shared checkout shows another session's
uncommitted work when you arrive, don't even create your branch yet: wait or ask, the same as the
read/write rule above.

**Branch protection (public repo `Matute289/xindeler-new-horizon`):**
- `main` and `development` require a PR + 1 approval, block force-pushes and deletion — but **`enforce_admins` is OFF**: Matias (as repo admin) can merge or push directly when he chooses to, unlike the sibling Bevy-port repo where even admins are hard-blocked. This is deliberate for this project.
- AI agents must still NEVER merge or approve PRs, push to `main`/`development`, or touch branch-protection settings themselves — the admin-bypass exists for Matias, not for the agent. Workflow: branch off `development` → commit → push branch → open PR with base `development` → stop and report. Only Matias reviews and merges (or bypasses, at his own discretion).

## Git LFS & Binary Assets (the VPS) — IMPORTANT

Large binary assets (`.vox`, `.png`/`.jpg`/`.jpeg`, `.ogg`/`.wav`, `.ttf`, `.ico`, `.obj`/`.blend`, `assets/world/map/*.bin`, etc. — the full list is `.gitattributes`) are **NOT stored on GitHub**. They live on a self-hosted Git LFS store on the VPS. GitHub holds only code, RON/i18n text, and tiny **LFS pointer files**.

**Topology — three sources, one working tree:**
- **GitHub public** (`Matute289/xindeler-new-horizon`, `origin`) — code + RON/i18n + LFS pointers. No blobs.
- **VPS** (`greenmountain.dev:/srv/git-lfs/repos/xindeler.git`) — the SAME shared blob store the sibling Bevy-port repo and `xindeler-old` already use (asset history is common to all three), served by `git-lfs-transfer` over **pure SSH** (no HTTP server, no Caddy). Private (SSH-key auth). It is the **single copy** of the binaries, so it must be backed up server-side. Server-side setup notes live in the private `MyServerVPS` repo (`git-lfs/`).
- **GitHub private** (`Matute289/xindeler-design`, nested at `docs/design/`) — design/lore, shared with the sibling Bevy-port repo (see above).

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
- **Server release** → built **on the VPS** (where the assets are local), not on GitHub Actions. `release.yml` triggers on a `v*` tag push, SSHes to the VPS with `secrets.VPS_SSH_KEY`, and runs `/srv/git-lfs/scripts/build-release.sh <tag>`, which checks out the tag in the live `/opt/xindeler-server/src` checkout and delegates the actual build/install/health-check/rollback to `deploy/deploy.sh` in this repo. Already adapted and proven for `xindeler-new-horizon` specifically (binary `xindeler-server-cli`, toolchain pin, `.lfsconfig` all match this repo) — v0.1.0 through v0.23.0 have shipped this way as of 2026-09.
- **Docker image** (`publish-docker.yml`, manual) → pulls only the asset dirs the image bundles (`assets/common,server,world`) from the VPS, builds `xindeler-server-cli`, pushes to GHCR.
- **Client release** (voxygen desktop installer + Airshipper) → **deferred**, same as the sibling repo.
- **Repo secrets**: `secrets.VPS_SSH_KEY` is configured (set 2026-09-05, using the same `~/.ssh/xindeler_ci` dedicated deploy key the sibling `xindeler` repo's identically-named secret already uses).

**GitHub Actions minutes:** the 2,000-minute quota is for **private** repos only; the public `xindeler-new-horizon` repo runs Actions for free. Heavy Rust builds run on the VPS anyway, so they don't consume GitHub minutes.

## Upstream Sync (GitLab Veloren)

Xindeler is a fork of `gitlab veloren/veloren` (the `gitlab` remote — fetch-only, never push). To pull upstream `master` and update without breaking or overwriting Xindeler's work:

- Two independent mechanisms, usable separately or in sequence — neither force-pushes `main`/`development`:
  - **`upstream-sync.yml`** (manual `workflow_dispatch`) checks GitLab for new commits and, if any exist, opens a **raw, non-mergeable draft PR** off an `upstream/review-YYYY-MM-DD-<sha>` branch — for eyeballing the upstream delta only, never merge it directly.
  - **The `gitlab-master-merger` skill** does the real integration: creates its own `upstream/integrate-YYYY-MM-DD` branch off `development`, merges `gitlab/master`, resolves conflicts, validates (`cargo check --workspace`, tests, clippy, fmt), and opens the actual mergeable PR. This is the one to run directly when asked to "bring in upstream changes" — it doesn't require `upstream-sync.yml` to have run first.
- ⚠️ **Never hard-mirror** upstream over our branches. (The old `mirror.yml` did `git push --force master→main` and was removed for exactly this reason; branch protection blocks it anyway.)
- Upstream brings its own LFS binaries — these route to the **VPS** via `.lfsconfig`, never to GitHub.
- After a sync, run the lint/test commands above and resolve conflicts so Xindeler customizations (classes, races, magic, lore-driven assets, CI/LFS config, etc.) are preserved — upstream must never clobber them.

## Build Profiles

Custom profiles in the workspace `Cargo.toml`:
- `dev` (default): opt-level=2, debug assertions on — faster iteration than a true debug build.
- `release`: opt-level=3, full LTO, `panic=abort`.
- `no_overflow`: Used in world-gen crates to skip overflow checks for performance.

## 📋 Project Backlog (scored & prioritized)

**The working backlog for this project is [`docs/design/backlog/new-horizon.md`](docs/design/backlog/new-horizon.md)** —
private (see Documentation & Git Policy above), `NH-N` rows, each referencing its specs/plans/tasks in
the same private `docs/design/` repo. This is now the **single backlog across every Xindeler repo**:
the sibling Bevy-port repo's `docs/backlog/backlog.md` (`BL-NN`) and `docs/backlog/engine-migration.md`
(`EM-N`) were merged in and deleted 2026-09-05 (see the `🎯 PRIORITY EXECUTION ORDER` section's
"Tercera/Cuarta pasada" notes and the BL-→NH- reconciliation ledger inside `new-horizon.md` for the
full mapping). There is still no public `docs/backlog/backlog.md` in *this* repo — if/when this
project reaches a maturity where a public-facing summary backlog is useful, model it after the
sibling repo's old `docs/backlog/engine-migration.md` shape, but don't invent one prematurely.

**Always read `docs/design/backlog/new-horizon.md` on resume and before starting / after finishing any
work.** The backlog is **multi-session**: `git pull`/re-sync `development` (in `docs/design`, a separate
repo — see Documentation & Git Policy) before editing, add+score new `NH-N` rows there, and commit only
your own rows.

