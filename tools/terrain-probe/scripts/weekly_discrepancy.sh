#!/usr/bin/env bash
# Weekly fast-path vs client-path discrepancy run on the research arena.
#
# Meant to run ON THE VPS (it has the Git LFS assets locally), triggered over
# SSH with the existing `VPS_SSH_KEY` pattern (see publish-docker.yml / the
# release.yml build-release step) or by hand. NOT a GitHub Actions job: code CI
# deliberately never pulls LFS. This script installs no cron/timer.
#
#   tools/terrain-probe/scripts/weekly_discrepancy.sh ASSETS_DIR [LOG_DIR]
#
# ASSETS_DIR: an `assets` directory holding the arena build of the Cromatolis
# map (research `ar_arena1/assets`; the arena only exists in that build).
# LOG_DIR (default $HOME/tprobe-weekly): one timestamped log + the dumps of the
# run; the newest log is also linked as `latest.log`, and the exit status of
# the last run is in `latest.status`.
#
# Exit status: 0 pass; 1 differences or too few chunks streamed; 2 environment
# or build failure (the same convention as arena_client_check.sh).
set -uo pipefail

ASSETS=${1:?usage: weekly_discrepancy.sh ASSETS_DIR [LOG_DIR]}
LOGS=${2:-$HOME/tprobe-weekly}
ROOT=$(cd "$(dirname "$0")/../../.." && pwd)
STAMP=$(date -u +%Y%m%dT%H%M%SZ)
RUN="$LOGS/$STAMP"
mkdir -p "$RUN"
LOG="$RUN/run.log"
ln -sfn "$LOG" "$LOGS/latest.log"

finish() { echo "$1" > "$LOGS/latest.status"; echo "exit status $1 (log $LOG)" | tee -a "$LOG"; exit "$1"; }

{
    echo "weekly discrepancy run $STAMP"
    echo "repo  $ROOT @ $(git -C "$ROOT" rev-parse HEAD 2>/dev/null || echo unknown)"
    echo "assets $ASSETS"
} | tee -a "$LOG"

[ -d "$ASSETS/world/map" ] || { echo "ASSETS_DIR has no world/map: $ASSETS" | tee -a "$LOG"; finish 2; }

cd "$ROOT" || finish 2
# Build both binaries from the same checkout so the commit-skew warning in the
# client dump stays quiet.
if ! cargo build -p xindeler-server-cli -p xindeler-terrain-probe --locked >>"$LOG" 2>&1; then
    echo "build failed" | tee -a "$LOG"
    finish 2
fi

TARGET=${CARGO_TARGET_DIR:-$ROOT/target}
TERRAIN_PROBE="$TARGET/debug/terrain-probe" TPROBE_SERVER_BIN="$TARGET/debug/xindeler-server-cli" \
    bash "$ROOT/tools/terrain-probe/scripts/arena_client_check.sh" "$ASSETS" "$RUN" >>"$LOG" 2>&1
status=$?
grep -E 'client dump:|discrepancy:|WARNING' "$LOG" | tail -5
finish "$status"
