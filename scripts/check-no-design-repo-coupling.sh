#!/bin/sh
# game-architecture guard — shipped engine code must NOT reference the private design repo.
#
# Data-driven / clean-architecture rule (see .agents/skills/game-architecture/SKILL.md):
# the engine depends only on shipped `assets/` data, NEVER on a `docs/…` design-repo path
# (specs/plans/lore live in the private repo and must not be a build/test dependency).
#
# Wire as a CI step or a git pre-commit hook once the codebase is clean of such coupling.
# Runnable by hand: scripts/check-no-design-repo-coupling.sh

set -eu
cd "$(git rev-parse --show-toplevel)"

# Design-repo paths that must not appear in shipped Rust (in code, not doc-comments).
pattern='docs/superpowers|docs/design|docs/xindeler|"\.\./lore"|join\("\.\./lore'

# Scan all tracked Rust outside generated/build dirs; drop pure `//` comment lines
# (a doc-comment pointing at a spec is fine; a runtime path into the design repo is not).
hits="$(grep -rnE "$pattern" --include='*.rs' --exclude-dir=target --exclude-dir=.git . 2>/dev/null \
        | grep -vE '^[^:]*:[0-9]+:[[:space:]]*//' || true)"

if [ -n "$hits" ]; then
    echo "✗ game-architecture: shipped engine code references the private design repo:" >&2
    echo "$hits" | sed 's/^/    /' >&2
    echo "" >&2
    echo "  Engine code must depend only on shipped assets/ data — never on a design-repo path." >&2
    echo "  Fix: depend on a generated assets/*.ron artifact, or defer the loader until a system" >&2
    echo "  actually consumes it (YAGNI). See .agents/skills/game-architecture/SKILL.md." >&2
    exit 1
fi

echo "✓ game-architecture: no engine↔design-repo coupling."
