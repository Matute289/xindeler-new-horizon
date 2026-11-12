#!/usr/bin/env bash
# Fast dump + client dump + diff on the synthetic research arena (centre wpos
# (23264, 25312), empty of sites), and print the discrepancy summary.
#
#   tools/terrain-probe/scripts/arena_client_check.sh ASSETS_DIR [OUT_DIR]
#
# ASSETS_DIR must be a directory named `assets` holding the arena build of the
# Cromatolis map (research `ar_arena1/assets`); on the real map this box is
# ordinary terrain, so the numbers only reproduce spec 4.5 on the arena.
# Needs `cargo build -p xindeler-terrain-probe -p xindeler-server-cli` first
# (override the binaries with TERRAIN_PROBE / TPROBE_SERVER_BIN).
# Exit: 0 pass, 1 differences or too few chunks streamed, 2 error.
set -euo pipefail

ASSETS=${1:?usage: arena_client_check.sh ASSETS_DIR [OUT_DIR]}
OUT=${2:-$(mktemp -d "${TMPDIR:-/tmp}/tprobe-arena-XXXXXX")}
ROOT=$(cd "$(dirname "$0")/../../.." && pwd)
TP=${TERRAIN_PROBE:-${CARGO_TARGET_DIR:-$ROOT/target}/debug/terrain-probe}
BOX=22750,24550,23950,25610
ZMIN=100
ZMAX=280

[ -x "$TP" ] || { echo "terrain-probe not built: $TP" >&2; exit 2; }
mkdir -p "$OUT"
echo "arena box $BOX z $ZMIN..$ZMAX, output in $OUT"

# Fast dump, client dump (which also runs the fast path again with
# --compare-fast and prints the discrepancy), then a standalone diff of the two
# files as an independent check of the saved dumps.
status=0
"$TP" --assets "$ASSETS" client-dump --box "$BOX" --zmin "$ZMIN" --zmax "$ZMAX" \
    --out "$OUT/arena_client.tprobe" --compare-fast --save-fast "$OUT/arena_fast.tprobe" \
    --keep-server-log || status=$?
echo
echo "== standalone diff of the saved dumps =="
"$TP" diff "$OUT/arena_fast.tprobe" "$OUT/arena_client.tprobe" --client-compare || status=$?
echo "exit status $status"
exit "$status"
