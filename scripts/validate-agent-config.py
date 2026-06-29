#!/usr/bin/env python3
"""Validate Xindeler's Codex/Claude skills and custom-agent mirrors."""

from __future__ import annotations

import ast
import json
import re
import sys
import tomllib
from pathlib import Path


ROOT = Path(__file__).resolve().parents[1]
CODEX_SKILLS = ROOT / ".agents" / "skills"
CLAUDE_SKILLS = ROOT / ".claude" / "skills"
CODEX_AGENTS = ROOT / ".codex" / "agents"
CLAUDE_AGENTS = ROOT / ".claude" / "agents"
HOOK_CONFIG = ROOT / ".codex" / "hooks.json"
HOOK_SCRIPTS = ROOT / ".codex" / "hooks"

FORBIDDEN_TEXT = {
    ".Codex/": "use `.agents/skills`, `.codex`, or a skill name",
    "RustroverProjects/veloren": "use a repository-relative path",
    "/path/to/veloren": "use an Xindeler example path",
    "docs/superpowers": "design documents live under `docs/design`",
    "Veloren fork": "describe the project as Xindeler",
    "fork of Veloren": "describe the project as Xindeler",
    "Veloren-based fork": "describe the project as Xindeler",
}


def parse_frontmatter(path: Path) -> tuple[dict[str, str], str]:
    text = path.read_text(encoding="utf-8")
    match = re.match(r"^---\n(.*?)\n---\n?", text, re.DOTALL)
    if not match:
        raise ValueError("missing YAML frontmatter")

    metadata: dict[str, str] = {}
    for line in match.group(1).splitlines():
        if ":" not in line:
            continue
        key, value = line.split(":", 1)
        metadata[key.strip()] = value.strip().strip("\"'")
    return metadata, text[match.end() :]


def validate_skills(errors: list[str]) -> None:
    codex = {p.parent.name: p for p in CODEX_SKILLS.glob("*/SKILL.md")}
    claude = {p.parent.name: p for p in CLAUDE_SKILLS.glob("*/SKILL.md")}

    if codex.keys() != claude.keys():
        errors.append(
            "skill mirrors differ: "
            f"Codex-only={sorted(codex.keys() - claude.keys())}, "
            f"Claude-only={sorted(claude.keys() - codex.keys())}"
        )

    for folder, codex_path in sorted(codex.items()):
        try:
            metadata, _ = parse_frontmatter(codex_path)
        except ValueError as error:
            errors.append(f"{codex_path.relative_to(ROOT)}: {error}")
            continue

        if metadata.get("name") != folder:
            errors.append(
                f"{codex_path.relative_to(ROOT)}: folder and `name` must match"
            )
        if not metadata.get("description"):
            errors.append(f"{codex_path.relative_to(ROOT)}: missing description")

        claude_path = claude.get(folder)
        if claude_path and codex_path.read_bytes() != claude_path.read_bytes():
            errors.append(f"skill mirror diverged: {folder}")


def validate_agents(errors: list[str]) -> None:
    codex = {p.stem: p for p in CODEX_AGENTS.glob("*.toml")}
    claude = {p.stem: p for p in CLAUDE_AGENTS.glob("*.md")}

    if codex.keys() != claude.keys():
        errors.append(
            "agent mirrors differ: "
            f"Codex-only={sorted(codex.keys() - claude.keys())}, "
            f"Claude-only={sorted(claude.keys() - codex.keys())}"
        )

    for stem, codex_path in sorted(codex.items()):
        try:
            config = tomllib.loads(codex_path.read_text(encoding="utf-8"))
        except (OSError, tomllib.TOMLDecodeError) as error:
            errors.append(f"{codex_path.relative_to(ROOT)}: invalid TOML: {error}")
            continue

        for field in ("name", "description", "developer_instructions"):
            if not config.get(field):
                errors.append(f"{codex_path.relative_to(ROOT)}: missing `{field}`")
        if config.get("name") != stem:
            errors.append(f"{codex_path.relative_to(ROOT)}: filename and `name` differ")
        if "read-only" in config.get("description", "").lower() and config.get(
            "sandbox_mode"
        ) != "read-only":
            errors.append(
                f"{codex_path.relative_to(ROOT)}: read-only agent lacks read-only sandbox"
            )

        claude_path = claude.get(stem)
        if not claude_path:
            continue
        try:
            metadata, body = parse_frontmatter(claude_path)
        except ValueError as error:
            errors.append(f"{claude_path.relative_to(ROOT)}: {error}")
            continue
        if metadata.get("name") != config.get("name"):
            errors.append(f"{stem}: agent names diverged")
        if metadata.get("description") != config.get("description"):
            errors.append(f"{stem}: agent descriptions diverged")
        if body.strip() != config.get("developer_instructions", "").strip():
            errors.append(f"{stem}: agent instructions diverged")


def validate_stale_text(errors: list[str]) -> None:
    paths = [
        *CODEX_SKILLS.glob("*/SKILL.md"),
        *CLAUDE_SKILLS.glob("*/SKILL.md"),
        *CODEX_AGENTS.glob("*.toml"),
        *CLAUDE_AGENTS.glob("*.md"),
    ]
    for path in paths:
        text = path.read_text(encoding="utf-8")
        for stale, guidance in FORBIDDEN_TEXT.items():
            if stale in text:
                errors.append(
                    f"{path.relative_to(ROOT)}: stale `{stale}`; {guidance}"
                )


def validate_hooks(errors: list[str]) -> None:
    try:
        config = json.loads(HOOK_CONFIG.read_text(encoding="utf-8"))
    except (OSError, json.JSONDecodeError) as error:
        errors.append(f"{HOOK_CONFIG.relative_to(ROOT)}: invalid JSON: {error}")
        return

    hooks = config.get("hooks")
    if not isinstance(hooks, dict):
        errors.append(f"{HOOK_CONFIG.relative_to(ROOT)}: missing `hooks` object")
    else:
        for event in ("PreToolUse", "Stop"):
            if not isinstance(hooks.get(event), list) or not hooks[event]:
                errors.append(
                    f"{HOOK_CONFIG.relative_to(ROOT)}: missing non-empty `{event}` hooks"
                )

    for path in sorted(HOOK_SCRIPTS.glob("*.py")):
        try:
            ast.parse(path.read_text(encoding="utf-8"), filename=str(path))
        except (OSError, SyntaxError) as error:
            errors.append(f"{path.relative_to(ROOT)}: invalid Python: {error}")


def main() -> int:
    errors: list[str] = []
    validate_skills(errors)
    validate_agents(errors)
    validate_stale_text(errors)
    validate_hooks(errors)
    if errors:
        print("Agent configuration validation failed:", file=sys.stderr)
        for error in errors:
            print(f"- {error}", file=sys.stderr)
        return 1

    skill_count = len(list(CODEX_SKILLS.glob("*/SKILL.md")))
    agent_count = len(list(CODEX_AGENTS.glob("*.toml")))
    print(f"Agent configuration valid: {skill_count} skills, {agent_count} agents")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
