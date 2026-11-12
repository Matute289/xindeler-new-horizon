#!/usr/bin/env python3
"""Stop hook: inside an isolated subagent worktree
(.claude/worktrees/agent-<id>/...), block stopping if the nested
docs/design repo has commits that aren't safely on GitHub yet -- local
main ahead of origin/main, or a branch with no pushed upstream / ahead of
its upstream.

docs/design is gitignored in the outer engine repo, so worktree
auto-cleanup (which only inspects the outer repo's diff) is blind to it:
an unpushed commit there is lost the moment the worktree is deleted. This
hook is the last line of defense for that specific failure mode.

Never fires for the main orchestrating session -- only inside a
dispatched subagent's own worktree (detected via the
.claude/worktrees/agent-<id> path segment).
"""

import json
import os
import subprocess
import sys

WORKTREE_MARKER = "/.claude/worktrees/agent-"


def run(args, cwd):
    try:
        return subprocess.run(args, cwd=cwd, capture_output=True, text=True, timeout=5)
    except Exception:
        return None


def count(args, cwd) -> int:
    res = run(args, cwd)
    if res is None or res.returncode != 0:
        return 0
    out = res.stdout.strip()
    return int(out) if out.isdigit() else 0


def main() -> int:
    try:
        payload = json.load(sys.stdin)
    except (json.JSONDecodeError, ValueError):
        return 0

    cwd = payload.get("cwd") or os.getcwd()
    if WORKTREE_MARKER not in cwd:
        return 0  # only guard dispatched subagent worktrees

    marker_idx = cwd.index(WORKTREE_MARKER) + len(WORKTREE_MARKER)
    agent_dir_name = cwd[marker_idx:].split("/", 1)[0]
    worktree_root = cwd[: cwd.index(WORKTREE_MARKER)] + WORKTREE_MARKER + agent_dir_name

    docs_design = os.path.join(worktree_root, "docs", "design")
    if not os.path.isdir(os.path.join(docs_design, ".git")) and not os.path.isfile(
        os.path.join(docs_design, ".git")
    ):
        return 0  # no nested repo here, nothing to guard

    branch_res = run(["git", "branch", "--show-current"], docs_design)
    if branch_res is None or branch_res.returncode != 0:
        return 0
    branch = branch_res.stdout.strip()
    if not branch:
        return 0  # detached HEAD / empty repo, nothing sane to check

    if branch == "main":
        ahead = count(["git", "rev-list", "--count", "origin/main..HEAD"], docs_design)
        if ahead > 0:
            print(
                f"BLOQUEADO: docs/design tiene {ahead} commit(s) locales en main "
                "sin pushear, en un worktree aislado a punto de cerrarse. "
                "docs/design es gitignored en el repo externo, asi que el "
                "auto-cleanup del worktree no lo detecta -- este commit se "
                "perderia. Arreglo:\n"
                "  cd docs/design\n"
                "  git branch <nombre-de-branch> HEAD\n"
                "  git reset --hard origin/main\n"
                "  git checkout <nombre-de-branch>\n"
                "  git push -u origin <nombre-de-branch>\n"
                "  gh pr create --base main --repo Matute289/xindeler-design",
                file=sys.stderr,
            )
            return 2
        return 0

    upstream_res = run(
        ["git", "rev-parse", "--abbrev-ref", f"{branch}@{{upstream}}"], docs_design
    )
    if upstream_res is None or upstream_res.returncode != 0:
        print(
            f"BLOQUEADO: la branch '{branch}' de docs/design no tiene upstream "
            "pusheado, en un worktree aislado a punto de cerrarse -- el commit "
            "se perderia al limpiarse el worktree. Arreglo:\n"
            f"  cd docs/design && git push -u origin {branch}\n"
            "  gh pr create --base main --repo Matute289/xindeler-design",
            file=sys.stderr,
        )
        return 2

    ahead = count(["git", "rev-list", "--count", f"{branch}@{{upstream}}..HEAD"], docs_design)
    if ahead > 0:
        print(
            f"BLOQUEADO: la branch '{branch}' de docs/design tiene {ahead} "
            "commit(s) sin pushear, en un worktree aislado a punto de "
            "cerrarse. Arreglo:\n"
            f"  cd docs/design && git push origin {branch}\n"
            "  (y confirma que el PR ya este abierto -- gh pr create --base "
            "main --repo Matute289/xindeler-design si todavia no existe)",
            file=sys.stderr,
        )
        return 2

    return 0


if __name__ == "__main__":
    sys.exit(main())
