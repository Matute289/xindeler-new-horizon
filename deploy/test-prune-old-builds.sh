#!/usr/bin/env bash
# Fixture tests for prune-old-builds.sh. Runs anywhere (no VPS, no docker):
#   bash deploy/test-prune-old-builds.sh
set -uo pipefail

HERE="$(cd "$(dirname "$0")" && pwd)"
SCRIPT="$HERE/prune-old-builds.sh"
T="$(mktemp -d)"
trap 'rm -rf "$T"' EXIT
FAILS=0

ok()   { echo "  ok   - $1"; }
fail() { echo "  FAIL - $1"; FAILS=$((FAILS + 1)); }
check() { # description, command...
    local d="$1"; shift
    if "$@"; then ok "$d"; else fail "$d"; fi
}

new_fixture() { # fresh sandbox in $T/<name>; echoes its path
    local r="$T/$1"
    mkdir -p "$r/rel" "$r/dl/releases" "$r/srv/src/target/release" "$r/srv/src/target/debug" "$r/lfs/repos"
    touch "$r/srv/xindeler-server-cli" "$r/srv/xindeler-server-cli.previous" "$r/lfs/repos/blob"
    echo "$r"
}
run() { # fixture, args...
    local r="$1"; shift
    PRUNE_RELEASES_DIR="$r/rel" PRUNE_DL_PUBLIC="$r/dl" PRUNE_SERVER_ROOT="$r/srv" \
    PRUNE_LFS_REPOS="$r/lfs/repos" PRUNE_LOCKFILE="$r/lock" PRUNE_SKIP_BUSY_CHECK=1 \
    bash "$SCRIPT" "$@" 2>&1
}
mk_tarballs() { local r="$1"; shift; for v in "$@"; do echo x > "$r/rel/xindeler-server-$v.tar.gz"; done; }
mk_dirs() { local r="$1"; shift; for v in "$@"; do mkdir -p "$r/dl/releases/$v"; echo x > "$r/dl/releases/$v/manifest.json"; done; }
latest() { printf '{"version": "%s"}\n' "$2" > "$1/dl/latest.json"; }
exists() { [ -e "$1" ]; }
absent() { [ ! -e "$1" ]; }

echo "1. semver ordering (0.9.0 < 0.10.0 < 0.26.1 < 1.0.0), not lexical, not mtime"
R="$(new_fixture semver)"
mk_tarballs "$R" v0.9.0 v0.10.0 v0.26.1 v1.0.0 v0.2.0
touch -t 203001010000 "$R/rel/xindeler-server-v0.2.0.tar.gz"   # newest mtime, oldest version
run "$R" --apply --only releases >/dev/null
check "kept v1.0.0"  exists "$R/rel/xindeler-server-v1.0.0.tar.gz"
check "kept v0.26.1" exists "$R/rel/xindeler-server-v0.26.1.tar.gz"
check "deleted v0.10.0" absent "$R/rel/xindeler-server-v0.10.0.tar.gz"
check "deleted v0.9.0"  absent "$R/rel/xindeler-server-v0.9.0.tar.gz"
check "deleted v0.2.0 despite newest mtime" absent "$R/rel/xindeler-server-v0.2.0.tar.gz"

echo "2. dry-run is the default and deletes nothing"
R="$(new_fixture dry)"
mk_tarballs "$R" v0.1.0 v0.2.0 v0.3.0; mk_dirs "$R" v0.1.0 v0.2.0 v0.3.0; latest "$R" v0.3.0
out="$(run "$R" --only releases,downloads,target)"
check "tarball survives" exists "$R/rel/xindeler-server-v0.1.0.tar.gz"
check "release dir survives" exists "$R/dl/releases/v0.1.0"
check "target/debug survives" exists "$R/srv/src/target/debug"
check "output says would delete" grep -q "would delete" <<<"$out"

echo "3. --incoming counts as one of the kept (deploy hook leaves new + previous)"
R="$(new_fixture incoming)"
mk_tarballs "$R" v0.24.0 v0.25.0 v0.26.0
run "$R" --apply --only releases --from-deploy --incoming v0.27.0 >/dev/null
check "kept previous v0.26.0" exists "$R/rel/xindeler-server-v0.26.0.tar.gz"
check "deleted v0.25.0" absent "$R/rel/xindeler-server-v0.25.0.tar.gz"
check "deleted v0.24.0" absent "$R/rel/xindeler-server-v0.24.0.tar.gz"
echo "   rebuild of an existing tag: incoming == newest existing"
R="$(new_fixture rebuild)"
mk_tarballs "$R" v0.25.0 v0.26.0
run "$R" --apply --only releases --incoming v0.26.0 >/dev/null
check "both kept" exists "$R/rel/xindeler-server-v0.25.0.tar.gz"

echo "4. fewer than 3 tarballs: nothing deleted"
R="$(new_fixture few)"
mk_tarballs "$R" v0.1.0 v0.2.0
run "$R" --apply --only releases >/dev/null
check "both kept" exists "$R/rel/xindeler-server-v0.1.0.tar.gz"

echo "5. downloads: keep latest.json version + previous"
R="$(new_fixture dl)"
mk_dirs "$R" v0.25.2 v0.25.4 v0.26.1 v0.9.0; latest "$R" v0.26.1
run "$R" --apply --only downloads >/dev/null
check "kept latest v0.26.1" exists "$R/dl/releases/v0.26.1"
check "kept previous v0.25.4" exists "$R/dl/releases/v0.25.4"
check "deleted v0.25.2" absent "$R/dl/releases/v0.25.2"
check "deleted v0.9.0" absent "$R/dl/releases/v0.9.0"

echo "6. downloads: latest.json older than a dir on disk (rollback / upload in flight)"
R="$(new_fixture dlnewer)"
mk_dirs "$R" v0.1.0 v0.2.0 v0.3.0 v0.4.0; latest "$R" v0.3.0
run "$R" --apply --only downloads >/dev/null
check "latest kept" exists "$R/dl/releases/v0.3.0"
check "previous kept" exists "$R/dl/releases/v0.2.0"
check "newer-than-latest kept" exists "$R/dl/releases/v0.4.0"
check "older deleted" absent "$R/dl/releases/v0.1.0"

echo "7. downloads: safety aborts delete nothing"
R="$(new_fixture bad1)"; mk_dirs "$R" v0.1.0 v0.2.0 v0.3.0
echo 'not json' > "$R/dl/latest.json"
run "$R" --apply --only downloads >/dev/null; rc=$?
check "unparseable latest.json -> exit 1" test "$rc" -eq 1
check "nothing deleted (unparseable)" exists "$R/dl/releases/v0.1.0"
R="$(new_fixture bad2)"; mk_dirs "$R" v0.1.0 v0.2.0 v0.3.0; latest "$R" v9.9.9
run "$R" --apply --only downloads >/dev/null; rc=$?
check "latest.json dir missing -> exit 1" test "$rc" -eq 1
check "nothing deleted (missing dir)" exists "$R/dl/releases/v0.1.0"
R="$(new_fixture bad3)"; mk_dirs "$R" v0.1.0 v0.2.0 v0.3.0; rm "$R/dl/latest.json" 2>/dev/null
run "$R" --apply --only downloads >/dev/null; rc=$?
check "no latest.json -> exit 1" test "$rc" -eq 1
check "nothing deleted (no latest.json)" exists "$R/dl/releases/v0.1.0"

echo "8. non-semver dirs/files are never deleted"
R="$(new_fixture odd)"
mk_dirs "$R" v0.1.0 v0.2.0 v0.3.0 v0.4.0-rc1 scratch; latest "$R" v0.3.0
mk_tarballs "$R" v0.1.0 v0.2.0 v0.3.0; echo x > "$R/rel/notes.txt"; echo x > "$R/rel/xindeler-server-vNEXT.tar.gz"
run "$R" --apply --only releases,downloads >/dev/null
check "rc dir kept" exists "$R/dl/releases/v0.4.0-rc1"
check "scratch dir kept" exists "$R/dl/releases/scratch"
check "notes.txt kept" exists "$R/rel/notes.txt"
check "odd tarball kept" exists "$R/rel/xindeler-server-vNEXT.tar.gz"

echo "9. target/debug removed, release + protected files + LFS store untouched"
R="$(new_fixture tgt)"
touch "$R/srv/src/target/debug/x" "$R/srv/src/target/release/y"
run "$R" --apply --only target >/dev/null
check "debug gone" absent "$R/srv/src/target/debug"
check "release kept" exists "$R/srv/src/target/release/y"
check "server binary kept" exists "$R/srv/xindeler-server-cli"
check ".previous kept" exists "$R/srv/xindeler-server-cli.previous"
check "LFS store kept" exists "$R/lfs/repos/blob"

echo "10. guard: safe_rm refuses protected paths even if config points at them"
R="$(new_fixture guard)"
PRUNE_RELEASES_DIR="$R/lfs/repos" PRUNE_DL_PUBLIC="$R/dl" PRUNE_SERVER_ROOT="$R/srv" \
PRUNE_LFS_REPOS="$R/lfs/repos" PRUNE_LOCKFILE="$R/lock" PRUNE_SKIP_BUSY_CHECK=1 \
    bash -c 'for v in v0.1.0 v0.2.0 v0.3.0; do echo x > "$PRUNE_RELEASES_DIR/xindeler-server-$v.tar.gz"; done; bash '"$SCRIPT"' --apply --only releases' >/dev/null 2>&1; rc=$?
check "refuses (exit 1) when releases dir is inside the LFS store" test "$rc" -eq 1
check "LFS tarball not deleted" exists "$R/lfs/repos/xindeler-server-v0.1.0.tar.gz"

echo "11. busy host is refused"
R="$(new_fixture busy)"; mk_tarballs "$R" v0.1.0 v0.2.0 v0.3.0
( exec -a cargo sleep 5 ) & sleep 0.3
PRUNE_RELEASES_DIR="$R/rel" PRUNE_DL_PUBLIC="$R/dl" PRUNE_SERVER_ROOT="$R/srv" PRUNE_LOCKFILE="$R/lock" \
    bash "$SCRIPT" --apply --only releases >/dev/null 2>&1; rc=$?
kill %1 2>/dev/null; wait 2>/dev/null
if pgrep -x cargo >/dev/null 2>&1 || [ "$rc" -eq 3 ]; then check "exit 3 while cargo runs" test "$rc" -eq 3; else ok "skipped (could not fake a cargo process)"; fi
check "nothing deleted while busy" exists "$R/rel/xindeler-server-v0.1.0.tar.gz"

echo
if [ "$FAILS" -eq 0 ]; then echo "ALL PASSED"; else echo "$FAILS FAILED"; exit 1; fi
