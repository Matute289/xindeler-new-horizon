#!/usr/bin/env python3
"""PreToolUse hook (Bash): blocks any `git commit` that would land on
docs/design's `main` branch.

docs/design is xindeler's nested, separate private repo
(Matute289/xindeler-design) and must always go through its own branch +
PR workflow -- see CLAUDE.md's "Documentation & Git Policy" section.
Never commit there directly to main.

Walks the command left-to-right tracking `cd`/`git -C` so it catches
`cd docs/design && git commit ...`, `git -C docs/design commit ...`, and
plain `git commit ...` when the shell's cwd is already inside docs/design.
"""

import json
import os
import re
import subprocess
import sys

CD_RE = re.compile(r"(?:^|\s)cd\s+(\S+)")
COMMIT_RE = re.compile(r"(?:^|\s)git(?:\s+-C\s+(\S+))?\s+commit\b")


def resolve(base: str, path: str) -> str:
    path = path.strip("'\"")
    if path.startswith("/"):
        return os.path.normpath(path)
    return os.path.normpath(os.path.join(base, path))


def find_commit_target(command: str, cwd: str):
    current = cwd
    target = None
    for segment in re.split(r"&&|\|\||;", command):
        segment = segment.strip()
        cd_match = CD_RE.search(segment)
        if cd_match:
            current = resolve(current, cd_match.group(1))
            continue
        commit_match = COMMIT_RE.search(segment)
        if commit_match:
            explicit = commit_match.group(1)
            target = resolve(current, explicit) if explicit else current
    return target


def main() -> int:
    try:
        payload = json.load(sys.stdin)
    except (json.JSONDecodeError, ValueError):
        return 0

    if payload.get("tool_name") != "Bash":
        return 0

    command = payload.get("tool_input", {}).get("command", "") or ""
    cwd = payload.get("cwd") or os.getcwd()

    target = find_commit_target(command, cwd)
    if not target or "docs/design" not in target.replace(os.sep, "/"):
        return 0

    try:
        result = subprocess.run(
            ["git", "-C", target, "branch", "--show-current"],
            capture_output=True,
            text=True,
            timeout=5,
        )
    except Exception:
        return 0  # can't verify -- fail open

    if result.returncode != 0:
        return 0

    branch = result.stdout.strip()
    if branch == "main":
        print(
            "BLOQUEADO: docs/design nunca se commitea directo a main.\n"
            "Es un repo privado separado (Matute289/xindeler-design) con su "
            "propio flujo de branch + PR (ver CLAUDE.md, seccion "
            "'Documentation & Git Policy').\n"
            "Arreglo: git checkout -b <nombre-de-branch>, commiteá ahi, "
            "pusheá (git push -u origin <branch>) y abri el PR "
            "(gh pr create --base main --repo Matute289/xindeler-design) "
            "ANTES de terminar -- un commit sin pushear en un worktree "
            "aislado se pierde solo cuando el worktree se limpia.",
            file=sys.stderr,
        )
        return 2

    return 0


if __name__ == "__main__":
    sys.exit(main())
