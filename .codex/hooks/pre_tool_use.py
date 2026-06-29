#!/usr/bin/env python3
"""PreToolUse guardrails for shell commands in the Xindeler workspace."""

from __future__ import annotations

import json
import os
import re
import shutil
import subprocess
import sys
from pathlib import Path


PROJECT_ROOT = Path(__file__).resolve().parents[2]
SHELL_BOUNDARY = r"(?:^|&&|\|\||[;|\n])\s*"
COMMAND_PREFIX = r"(?:(?:env|sudo|command)\s+)*"

GIT_COMMIT_RE = re.compile(
    SHELL_BOUNDARY
    + COMMAND_PREFIX
    + r"(?:\S*/)?git(?:\s+(?:-[A-Za-z]+|--\S+|-C\s+\S+))*\s+commit(?:\s|$)"
)
RM_RE = re.compile(
    SHELL_BOUNDARY + COMMAND_PREFIX + r"(?:\S*/)?rm(?:\s|$)(?P<args>[^;|\n]*)"
)
SQLITE_RE = re.compile(
    SHELL_BOUNDARY + COMMAND_PREFIX + r"(?:\S*/)?sqlite3(?:\s|$)"
)
CRITICAL_RM_TARGET_RE = re.compile(
    r"(?i)(?:^|[/\s\"'])"
    r"(?:assets|persistence|userdata)"
    r"(?:[/\s\"'*]|$)|\.sqlite(?:3)?(?:[/\s\"'*]|$)"
)


def deny(message: str) -> None:
    print(message, file=sys.stderr)
    raise SystemExit(2)


def read_command() -> str:
    try:
        payload = json.load(sys.stdin)
    except (json.JSONDecodeError, OSError) as error:
        deny(f"BLOQUEADO: Codex envió una entrada de hook inválida: {error}")

    tool_input = payload.get("tool_input", {})
    command = tool_input.get("command") if isinstance(tool_input, dict) else None
    if not isinstance(command, str):
        deny("BLOQUEADO: El hook no pudo identificar el comando de shell.")
    return command


def check_critical_removal(command: str) -> None:
    for match in RM_RE.finditer(command):
        if CRITICAL_RM_TARGET_RE.search(match.group("args")):
            deny(
                "BLOQUEADO: rm sobre una ruta crítica de Xindeler "
                "(assets/, *.sqlite, persistence/ o userdata/). "
                "Requiere confirmación explícita del usuario."
            )


def check_direct_sqlite(command: str) -> None:
    if SQLITE_RE.search(command):
        deny(
            "BLOQUEADO: Operación directa con sqlite3 detectada. "
            "La base de datos de Xindeler requiere aprobación explícita."
        )


def cargo_executable() -> str:
    cargo = shutil.which("cargo")
    if cargo:
        return cargo

    fallback = Path.home() / ".cargo" / "bin" / "cargo"
    if fallback.is_file() and os.access(fallback, os.X_OK):
        return str(fallback)

    deny("BLOQUEADO: No se encontró cargo; no se pudo verificar el formato Rust.")
    raise AssertionError("deny() does not return")


def check_rust_format(command: str) -> None:
    if not GIT_COMMIT_RE.search(command):
        return

    result = subprocess.run(
        [cargo_executable(), "fmt", "--all", "--", "--check"],
        cwd=PROJECT_ROOT,
        text=True,
        capture_output=True,
        check=False,
    )
    if result.returncode == 0:
        return

    if result.stdout:
        print(result.stdout, file=sys.stderr, end="")
    if result.stderr:
        print(result.stderr, file=sys.stderr, end="")
    deny(
        "BLOQUEADO: Hay archivos Rust con formato incorrecto. "
        "Corré `cargo fmt --all` desde la raíz de Xindeler."
    )


def main() -> None:
    command = read_command()
    check_critical_removal(command)
    check_direct_sqlite(command)
    check_rust_format(command)


if __name__ == "__main__":
    main()
