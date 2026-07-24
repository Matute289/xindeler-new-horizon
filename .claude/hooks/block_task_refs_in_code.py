#!/usr/bin/env python3
"""PreToolUse hook: blocks Edit/Write/MultiEdit calls that add code comments
referencing backlog/task/plan/PR bookkeeping (BL-NN, EM-N.N, T-digit.digit,
PR #NNN, worksheet/spec/plan/tasks doc paths, etc.) instead of just
explaining what the code does.

Comments should document the CODE (what a function/struct/method does, why,
error handling, invariants) — never the TASK that produced it. That
narrative belongs in commit messages, PR descriptions, and docs/design/,
not in the source tree.

Only inspects comment-like lines in source files (.rs, .ron, .ftl); doc
files (.md, .txt) and non-comment code are left alone.
"""

import json
import re
import sys

# File extensions this rule applies to (source/data files, not docs).
CODE_EXTENSIONS = (".rs", ".ron", ".ftl")

# Patterns that indicate a task/backlog/plan/PR reference rather than an
# explanation of the code itself. Word-boundary-anchored and specific to
# this project's own tracking conventions, to minimize false positives on
# ordinary descriptive comments.
FORBIDDEN_PATTERNS = [
    r"\bBL-\d+\b",  # backlog item, e.g. BL-82
    r"\bNH-\d+[a-z]?\b",  # new-horizon backlog item, e.g. NH-4, NH-99b
    r"\bEM-\d+(?:\.\d+)*[a-z]?\b",  # engine-migration sub-item, e.g. EM-5.16, EM-5.16b
    r"\bT\d+\.\d+[a-z]?\b",  # task-board task number, e.g. T56.43
    r"\bPR\s*#\d+\b",  # PR reference, e.g. PR #207
    r"\bworksheet\b",
    r"\bdocs/design/(specs|plans|tasks)/",  # private design-repo doc paths
    r"\bPhase\s+\d+\b",  # migration phase reference
    r"\b[Mm]at[íi]as\b",  # attribution to the project owner by name
]

FORBIDDEN_RE = re.compile("|".join(FORBIDDEN_PATTERNS))

# Comment-line prefixes per file type (checked against the stripped line).
COMMENT_PREFIXES = ("//", "///", "//!", "/*", "*", "#")


def is_comment_line(line: str) -> bool:
    stripped = line.strip()
    return stripped.startswith(COMMENT_PREFIXES)


def find_violations(text: str, file_path: str) -> list[str]:
    if not file_path.endswith(CODE_EXTENSIONS):
        return []
    violations = []
    for line in text.splitlines():
        if not is_comment_line(line):
            continue
        match = FORBIDDEN_RE.search(line)
        if match:
            violations.append(f"{match.group(0)!r} in: {line.strip()[:120]}")
    return violations


def main() -> int:
    try:
        payload = json.load(sys.stdin)
    except (json.JSONDecodeError, ValueError):
        return 0  # can't parse, don't block

    tool_name = payload.get("tool_name", "")
    tool_input = payload.get("tool_input", {})

    texts_and_path = []
    if tool_name == "Edit":
        path = tool_input.get("file_path", "")
        texts_and_path.append((tool_input.get("new_string", ""), path))
    elif tool_name == "Write":
        path = tool_input.get("file_path", "")
        texts_and_path.append((tool_input.get("content", ""), path))
    elif tool_name == "MultiEdit":
        path = tool_input.get("file_path", "")
        for edit in tool_input.get("edits", []):
            texts_and_path.append((edit.get("new_string", ""), path))
    else:
        return 0

    all_violations = []
    for text, path in texts_and_path:
        all_violations.extend(find_violations(text, path))

    if all_violations:
        msg = (
            "BLOQUEADO: el comentario que estás por escribir referencia una "
            "tarea/epica/plan/PR en vez de explicar el código.\n"
            "Reglas del proyecto: los comentarios documentan QUÉ hace el código "
            "(función, struct, método, invariante, manejo de errores) — nunca "
            "de qué tarea/epica/PR/plan salió. Esa narrativa va en el mensaje "
            "de commit, la descripción del PR, o docs/design/ — no en el código.\n"
            "Reescribí el comentario sin la referencia y reintentá.\n"
            "Coincidencias encontradas:\n  - " + "\n  - ".join(all_violations)
        )
        print(msg, file=sys.stderr)
        return 2

    return 0


if __name__ == "__main__":
    sys.exit(main())
