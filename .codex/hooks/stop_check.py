#!/usr/bin/env python3
"""Warn when a Codex turn ends with unverified Rust changes."""

from __future__ import annotations

import json
import subprocess
from pathlib import Path


PROJECT_ROOT = Path(__file__).resolve().parents[2]


def main() -> None:
    result = subprocess.run(
        ["git", "status", "--porcelain=v1", "--untracked-files=all"],
        cwd=PROJECT_ROOT,
        text=True,
        capture_output=True,
        check=False,
    )
    if result.returncode != 0:
        return

    rust_changes = [
        line for line in result.stdout.splitlines() if ".rs" in line.removeprefix("?? ")
    ]
    if not rust_changes:
        return

    print(
        json.dumps(
            {
                "continue": True,
                "systemMessage": (
                    "Hay archivos .rs modificados o nuevos sin una verificación "
                    "final registrada. Antes de entregar cambios Rust, ejecutá "
                    "`cargo ci-clippy` o la comprobación proporcional indicada "
                    "por AGENTS.md."
                ),
            },
            ensure_ascii=False,
        )
    )


if __name__ == "__main__":
    main()
