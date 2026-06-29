---
name: xindeler-repo-policy
description: Use when committing, pushing, merging, opening a PR, creating branches, or saving/writing any documentation (specs, plans, task boards, brainstorm output, lore) in this repo — before running the git/gh command, not after
---

# Repo Layout & Git Policy (xindeler)

## Two repos, one working tree

| Path | Repo | Visibility | Rule |
|---|---|---|---|
| `/` (code, assets, `.agents/`, `.codex/`, `.claude/`) | `Matute289/xindeler` (`origin`) | PUBLIC | Feature branch + PR only |
| `docs/design/` | `Matute289/xindeler-design` (nested git repo) | PRIVATE | Commit/push from inside that dir |
| `docs/design/lore/` | `Matute289/xindeler-design` (nested repo) | PRIVATE | Canonical lore home; commit/push from inside `docs/design/` |
| `docs/lore/` | none — legacy path, kept gitignored as a guard | — | Do NOT create files here; lore goes in `docs/design/lore/` |
| `.superpowers/`, `graphify-out/` | local scratch, gitignored | — | Never commit anywhere |
| `gitlab` remote | upstream `veloren/veloren` (push disabled) | — | Fetch only, never push |

## Where each document goes

- Specs → `docs/design/specs/YYYY-MM-DD-<name>-design.md`
- Implementation plans → `docs/design/plans/YYYY-MM-DD-<name>.md`
- Task boards → `docs/design/tasks/NN-<name>-tasks.md` (index: `00-task-board.md`)
- Lore canon (markdown) → `docs/design/lore/` (structure per the lore-cosmology spec)
- After editing design docs: `cd docs/design && git add -A && git commit && git push` — it is a SEPARATE repo; committing from the repo root is a silent no-op (the path is gitignored there).
- Design content (specs, plans, brainstorms, balance notes) must NEVER appear in a public-repo commit.

## Branch protection (public repo)

`main` and `development`: PR required + 1 approval, enforced for admins, force-push and deletion blocked.

**Hard rules for AI agents — no exceptions:**
- NEVER push directly to `main` or `development`. The push will be rejected; do not look for workarounds.
- NEVER merge or approve a PR. Only Matias merges, after his review.
- NEVER modify branch-protection settings or use admin APIs to bypass them.
- Standard workflow: branch off `development` → commit → push branch → `gh pr create --base development` → STOP and report the PR URL.
- `main` only receives promotions from `development`, also via PR.

## Common mistakes

| Mistake | Fix |
|---|---|
| `git add docs/design` from repo root | No-op (gitignored). Commit inside `docs/design/`. |
| Committing `.superpowers/` brainstorm scratch | Gitignored on purpose. Distill conclusions into a spec/plan instead. |
| PR with base `main` for feature work | Base is `development`. |
| Merging own PR "because tests pass" | Never. Report the URL and stop. |
