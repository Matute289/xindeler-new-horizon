#!/usr/bin/env bash
#
# Deploys the game server on the VPS.
#
#   bash /opt/xindeler-server/src/deploy/deploy.sh          # latest development
#   bash /opt/xindeler-server/src/deploy/deploy.sh v0.19.0  # a specific tag/ref
#
# Modelled directly on xindeler-auth's deploy/deploy.sh (same VPS, same
# systemd/sudoers shape). Takes a database + rtsim-save backup, keeps the
# previous binary, health-checks after restarting, and restores the previous
# binary automatically if the new one does not come up.
#
# No deploy script existed for this repo before this one -- the VPS binary
# was updated by hand, in place, with no rollback path. The tagged-release
# path (`build-release.sh` on the VPS, triggered by a `v*` tag push via
# release.yml) reuses this same script rather than duplicating its build/
# install/health-check/rollback logic in a second, separately-maintained
# copy -- see that script's own header for why.
#
# An optional $1 is a ref (tag or branch) to check out instead of pulling
# `development`'s tip. Passing a tag leaves $SRC in detached HEAD; the next
# no-arg run needs a branch checked out first (`build-release.sh` handles
# this itself after a tagged build).

set -euo pipefail

REF="${1:-}"
ROOT="${SERVER_ROOT:-/opt/xindeler-server}"
SRC="$ROOT/src"
BIN="$ROOT/xindeler-server-cli"
PREVIOUS="$ROOT/xindeler-server-cli.previous"
SAVES="$ROOT/userdata/server/saves"
RTSIM="$ROOT/userdata/server/rtsim"
HEALTH_URL="${SERVER_HEALTH_URL:-http://127.0.0.1:14005/health}"
HEALTH_TIMEOUT=60
CARGO="${CARGO:-$HOME/.cargo/bin/cargo}"
SERVICE_UNIT="${SERVER_SERVICE_UNIT:-xindeler-server-cli.service}"
SUDOERS_HINT="mgrinberg ALL=(root) NOPASSWD: /usr/bin/systemctl restart $SERVICE_UNIT"

log() { echo "[deploy] $*"; }
fail() { echo "[deploy] ERROR: $*" >&2; exit 1; }

# Restarts the service.
#
# Prefers systemd, which sends SIGTERM and lets the server drain in-flight
# connections and finish its own shutdown sequence
# (server-cli/src/shutdown_coordinator.rs). Falls back to SIGKILL, which
# systemd sees as a failure and so triggers Restart=on-failure.
#
# SERVICE_UNIT must match the sudoers rule *character for character*,
# including the .service suffix -- see xindeler-auth/deploy/deploy.sh's own
# comment on this, the failure mode is identical here (silent fallback to
# SIGKILL, still works, just noisier in the logs).
restart_service() {
    if sudo -n systemctl restart "$SERVICE_UNIT" 2>/dev/null; then
        log "restarted via systemd (graceful shutdown)"
    else
        log "no passwordless sudo for '$SERVICE_UNIT'; restarting via SIGKILL"
        log "  (grant it with: $SUDOERS_HINT)"
        pkill -9 -f "xindeler-server-cli$" || true
    fi
}

wait_until_healthy() {
    local deadline=$((SECONDS + HEALTH_TIMEOUT))
    while [ $SECONDS -lt $deadline ]; do
        if curl -fsS --max-time 2 "$HEALTH_URL" >/dev/null 2>&1; then
            return 0
        fi
        sleep 1
    done
    return 1
}

rollback() {
    log "restoring the previous binary"
    [ -f "$PREVIOUS" ] || fail "no previous binary to restore; recover manually"
    cp "$PREVIOUS" "$BIN.restoring"
    mv "$BIN.restoring" "$BIN"
    restart_service
    if wait_until_healthy; then
        fail "deploy failed and was rolled back; the previous build is serving"
    fi
    fail "deploy failed AND rollback did not come up; service is DOWN"
}

# --- preflight ---------------------------------------------------------------

[ -d "$SRC" ] || fail "missing source checkout at $SRC"
[ -f "$ROOT/.env" ] || fail "missing $ROOT/.env"
[ -x "$CARGO" ] || fail "cargo not found at $CARGO"
command -v curl >/dev/null || fail "curl is required for the health check"

log "deploying to $ROOT"

# --- database + rtsim-save backup --------------------------------------------

# sqlite3 is not installed on the VPS (same as xindeler-auth), so ".backup" is
# unavailable. Copying the database together with its -wal and -shm siblings
# is the correct alternative: SQLite can recover the full state from that
# set. Copy the WAL *after* the database so it can only ever be newer, never
# staler, than the main file.
#
# rtsim's save (userdata/server/rtsim/data.dat) is backed up too, unlike
# auth's script -- this deploy can jump many migrations at once (BL-83: ~44
# PRs behind), and a bad rtsim-format change is a real, if unlikely, risk
# worth a cheap safety copy.
stamp="$(date -u +%Y%m%d-%H%M%S)"
if [ -f "$SAVES/db.sqlite" ]; then
    backup="$SAVES/db-pre-deploy-$stamp.sqlite"
    log "backing up the database to $(basename "$backup")"
    cp "$SAVES/db.sqlite" "$backup"
    [ -f "$SAVES/db.sqlite-wal" ] && cp "$SAVES/db.sqlite-wal" "$backup-wal"
    [ -f "$SAVES/db.sqlite-shm" ] && cp "$SAVES/db.sqlite-shm" "$backup-shm"
else
    log "no database yet; skipping database backup"
fi
if [ -f "$RTSIM/data.dat" ]; then
    rtsim_backup="$RTSIM/data-pre-deploy-$stamp.dat"
    log "backing up the rtsim save to $(basename "$rtsim_backup")"
    cp "$RTSIM/data.dat" "$rtsim_backup"
else
    log "no rtsim save yet; skipping rtsim backup"
fi

# --- build -------------------------------------------------------------------

cd "$SRC"
previous_commit="$(git rev-parse --short HEAD)"
log "current commit $previous_commit"

if [ -n "$REF" ]; then
    log "fetching and checking out $REF"
    git fetch origin --tags
    git checkout "$REF"
else
    log "pulling latest code"
    git pull --ff-only
fi

new_commit="$(git rev-parse --short HEAD)"
if [ "$previous_commit" = "$new_commit" ]; then
    log "already at $new_commit; rebuilding anyway"
else
    log "updated $previous_commit -> $new_commit"
fi

# No `+toolchain` override, unlike xindeler-auth's `+stable`: this repo pins
# nightly via its own `rust-toolchain` file (the `specs` ECS crate requires
# it), and rustup resolves that automatically from $SRC. `default-publish`
# disables hot-reloading (the dylib-loaded animation/agent code has no place
# in a release binary) -- see CLAUDE.md's own release-build command.
log "building release binary (this takes ~30 min on 2 vCPU)"
"$CARGO" build --release --locked --no-default-features --features default-publish -p xindeler-server-cli

built="$SRC/target/release/xindeler-server-cli"
[ -x "$built" ] || fail "build did not produce $built"

# --- install -----------------------------------------------------------------

if [ -f "$BIN" ]; then
    log "keeping the current binary as $(basename "$PREVIOUS") for rollback"
    cp "$BIN" "$PREVIOUS"
fi

log "installing the new binary"
cp "$built" "$BIN.new"
mv "$BIN.new" "$BIN"

restart_service

# --- verify ------------------------------------------------------------------

log "waiting for the service to answer on $HEALTH_URL"
if ! wait_until_healthy; then
    log "service did not become healthy within ${HEALTH_TIMEOUT}s"
    rollback
fi
log "service is up"

# Informational only. The health check above already proved the service is
# answering; process introspection is a nicety and must not fail the deploy.
pid="$(pgrep -f 'xindeler-server-cli$' | head -1 || true)"
if [ -n "$pid" ]; then
    threads="$(ps -o nlwp= -p "$pid" 2>/dev/null | tr -d ' ' || true)"
    log "running as pid $pid${threads:+ with $threads threads}"
fi

log "deployed $new_commit successfully"
log "rollback if needed: cp $PREVIOUS $BIN && pkill -9 -f 'xindeler-server-cli\$'"
