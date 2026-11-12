#!/usr/bin/env bash
#
# Retention / cleanup for the Xindeler VPS build artifacts.
#
# Policy (Matias): only the deployed version and the previous one are worth
# keeping (previous = rollback if the new one breaks). Everything older goes.
#
#   prune-old-builds.sh                  # DRY RUN (default): prints what would go
#   prune-old-builds.sh --apply          # actually delete
#   prune-old-builds.sh --apply --only releases --from-deploy --incoming v0.27.0
#
# Parts (select with --only a,b ; default = all):
#   releases   /srv/git-lfs/releases/xindeler-server-v*.tar.gz -> newest 2 by semver
#   downloads  /srv/xindeler/downloads/public/releases/<tag>/  -> the tag named in
#              latest.json + the previous one by semver
#   target     /opt/xindeler-server/src/target/debug           -> removed (release/ kept)
#   docker     dangling images + build cache                   -> pruned
#
# Hard guards (see safe_rm): never touches xindeler-server-cli, its .previous,
# portrait_gen, or anything under /srv/git-lfs/repos (the LFS blob store -- the
# SINGLE copy of the game's binary assets). Never deletes when fewer than 2
# candidate versions would remain.
#
# Exit codes: 0 ok, 1 error / aborted by a safety check, 2 usage,
#             3 refused because a build/deploy is in progress or another prune runs.

set -euo pipefail

# --- configuration (env-overridable, mostly for the test script) -------------

RELEASES_DIR="${PRUNE_RELEASES_DIR:-/srv/git-lfs/releases}"
DL_PUBLIC="${PRUNE_DL_PUBLIC:-/srv/xindeler/downloads/public}"
SERVER_ROOT="${PRUNE_SERVER_ROOT:-/opt/xindeler-server}"
LFS_REPOS="${PRUNE_LFS_REPOS:-/srv/git-lfs/repos}"
KEEP=2

APPLY=0
ONLY="releases,downloads,target,docker"
FROM_DEPLOY=0
INCOMING=""

usage() {
    sed -n '3,22p' "$0" | sed 's/^# \{0,1\}//'
    exit 2
}

while [ $# -gt 0 ]; do
    case "$1" in
        --apply) APPLY=1 ;;
        --dry-run) APPLY=0 ;;
        --only) shift; [ $# -gt 0 ] || usage; ONLY="$1" ;;
        --from-deploy) FROM_DEPLOY=1 ;;
        --incoming) shift; [ $# -gt 0 ] || usage; INCOMING="$1" ;;
        -h|--help) usage ;;
        *) echo "unknown argument: $1" >&2; usage ;;
    esac
    shift
done

want() { case ",$ONLY," in *",$1,"*) return 0 ;; *) return 1 ;; esac; }

for p in ${ONLY//,/ }; do
    case "$p" in releases|downloads|target|docker) ;; *) echo "unknown part: $p" >&2; usage ;; esac
done

SEMVER_RE='^v([0-9]+)\.([0-9]+)\.([0-9]+)$'
if [ -n "$INCOMING" ] && ! [[ "$INCOMING" =~ $SEMVER_RE ]]; then
    echo "--incoming must look like v1.2.3, got '$INCOMING'" >&2
    exit 2
fi

# --- logging ----------------------------------------------------------------

MODE_TAG="dry-run"; [ "$APPLY" -eq 1 ] && MODE_TAG="apply"

# Under systemd stdout already lands in journald; avoid double entries there.
log() {
    local msg="[prune:$MODE_TAG] $*"
    echo "$msg"
    if [ -z "${JOURNAL_STREAM:-}" ] && command -v logger >/dev/null 2>&1; then
        logger -t xindeler-prune -- "$msg" || true
    fi
}
warn() { log "WARN: $*"; }
die() { log "ERROR: $*"; exit 1; }

human() { # KiB -> human string
    awk -v k="$1" 'BEGIN{ if (k>=1048576) printf "%.1f GiB", k/1048576; else if (k>=1024) printf "%.1f MiB", k/1024; else printf "%d KiB", k }'
}
size_kb() { du -sk "$1" 2>/dev/null | awk '{print $1}'; }

# --- lock + in-progress detection -------------------------------------------

LOCKFILE="${PRUNE_LOCKFILE:-/run/lock/xindeler-prune.lock}"
[ -w "$(dirname "$LOCKFILE")" ] || LOCKFILE="/tmp/xindeler-prune.lock"
exec 9>"$LOCKFILE"
if command -v flock >/dev/null 2>&1; then
    if ! flock -n 9; then
        log "another xindeler-prune is already running (lock $LOCKFILE); exiting"
        exit 3
    fi
else
    # flock(1) is util-linux (always on the VPS); this branch only serves dev
    # machines without it (macOS running the test script).
    warn "flock not found; running WITHOUT the exclusive lock"
fi

# When called from deploy.sh itself (--from-deploy) the caller IS the deploy,
# so the process check would always trip on it; the caller has already finished
# building. Everything else (timer, manual) must see an idle host.
if [ "$FROM_DEPLOY" -eq 0 ] && [ -z "${PRUNE_SKIP_BUSY_CHECK:-}" ]; then
    busy=""
    pgrep -x cargo >/dev/null 2>&1 && busy="$busy cargo"
    pgrep -x rustc >/dev/null 2>&1 && busy="$busy rustc"
    pgrep -f '(^|/)deploy\.sh( |$)' >/dev/null 2>&1 && busy="$busy deploy.sh"
    pgrep -f '(^|/)build-release\.sh( |$)' >/dev/null 2>&1 && busy="$busy build-release.sh"
    pgrep -f 'docker( buildx)? build' >/dev/null 2>&1 && busy="$busy docker-build"
    pgrep -f 'rsync --server.*downloads/public' >/dev/null 2>&1 && busy="$busy rsync-publish"
    if [ -n "$busy" ]; then
        log "build/deploy in progress (${busy# }); refusing to run"
        exit 3
    fi
fi

# --- safe deletion ----------------------------------------------------------

ALLOWED_ROOTS=("$RELEASES_DIR" "$DL_PUBLIC/releases" "$SERVER_ROOT/src/target/debug")
FREED_KB=0

# The ONE place deletion happens. Refuses anything outside the allowed roots or
# matching a protected name/path, regardless of what the callers computed.
safe_rm() {
    local path="$1" ok=0 root base
    case "$path" in *..*|"") die "refusing suspicious path '$path'" ;; esac
    base="$(basename "$path")"
    case "$base" in
        xindeler-server-cli|xindeler-server-cli.previous|portrait_gen|.env)
            die "refusing to delete protected file $path" ;;
    esac
    case "$path" in "$LFS_REPOS"|"$LFS_REPOS"/*) die "refusing to touch the LFS store: $path" ;; esac
    for root in "${ALLOWED_ROOTS[@]}"; do
        case "$path" in "$root"/*|"$root") ok=1 ;; esac
    done
    [ "$ok" -eq 1 ] || die "refusing to delete outside allowed roots: $path"
    [ -e "$path" ] || return 0
    local kb; kb="$(size_kb "$path")"; kb="${kb:-0}"
    if [ "$APPLY" -eq 1 ]; then
        rm -rf -- "$path"
        log "deleted $path ($(human "$kb"))"
    else
        log "would delete $path ($(human "$kb"))"
    fi
    FREED_KB=$((FREED_KB + kb))
}

semver_key() { # v1.2.3 -> zero-padded sortable key
    [[ "$1" =~ $SEMVER_RE ]] || return 1
    printf '%06d%06d%06d' "$((10#${BASH_REMATCH[1]}))" "$((10#${BASH_REMATCH[2]}))" "$((10#${BASH_REMATCH[3]}))"
}

# Sorts version names (one per line on stdin) newest-first by semver, not mtime.
sort_versions_desc() {
    local v k
    while IFS= read -r v; do
        [ -n "$v" ] || continue
        k="$(semver_key "$v")" || continue
        echo "$k $v"
    done | sort -r | awk '{print $2}'
}

# --- disk usage -------------------------------------------------------------

report_usage() {
    log "--- disk usage ($1) ---"
    local d
    for d in "$RELEASES_DIR" "$DL_PUBLIC" "$SERVER_ROOT/src/target"; do
        [ -d "$d" ] && log "  $(printf '%-9s' "$(human "$(size_kb "$d")")") $d"
    done
    local l
    l="$(df -h / 2>/dev/null | awk 'NR==2 {print "filesystem /: " $3 " used, " $4 " free (" $5 " full)"}')"
    [ -z "$l" ] || log "  $l"
}

# --- part: server tarballs --------------------------------------------------

prune_releases() {
    log "== releases: $RELEASES_DIR (keep newest $KEEP by semver) =="
    [ -d "$RELEASES_DIR" ] || { warn "$RELEASES_DIR missing; skipping"; return 0; }
    local f name v versions=()
    for f in "$RELEASES_DIR"/xindeler-server-v*.tar.gz; do
        [ -e "$f" ] || continue
        name="$(basename "$f")"
        v="${name#xindeler-server-}"; v="${v%.tar.gz}"
        if [[ "$v" =~ $SEMVER_RE ]]; then versions+=("$v"); else warn "ignoring non-semver file $name"; fi
    done
    # build-release.sh packs the new tarball AFTER deploy.sh returns, so when we
    # are called from deploy.sh the incoming version does not exist yet: count it
    # as one of the kept so the end state is exactly new + previous.
    local all=(${versions[@]+"${versions[@]}"})
    if [ -n "$INCOMING" ]; then
        local seen=0
        for v in ${versions[@]+"${versions[@]}"}; do [ "$v" = "$INCOMING" ] && seen=1; done
        [ "$seen" -eq 1 ] || all+=("$INCOMING")
    fi
    local sorted=() line
    while IFS= read -r line; do sorted+=("$line"); done < <(printf '%s\n' ${all[@]+"${all[@]}"} | sort_versions_desc)
    log "  candidates (newest first): ${sorted[*]:-none}"
    if [ "${#sorted[@]}" -le "$KEEP" ]; then
        log "  nothing to delete (${#sorted[@]} <= $KEEP versions)"
        return 0
    fi
    local i
    for ((i = KEEP; i < ${#sorted[@]}; i++)); do
        v="${sorted[$i]}"
        [ "$v" = "$INCOMING" ] && continue  # never delete the one being built
        safe_rm "$RELEASES_DIR/xindeler-server-$v.tar.gz"
    done
    log "  kept: ${sorted[*]:0:$KEEP}"
}

# --- part: client downloads -------------------------------------------------

read_latest_version() {
    local j="$1" v=""
    if command -v jq >/dev/null 2>&1; then
        v="$(jq -er '.version' "$j" 2>/dev/null)" || return 1
    elif command -v python3 >/dev/null 2>&1; then
        v="$(python3 -c 'import json,sys; print(json.load(open(sys.argv[1]))["version"])' "$j" 2>/dev/null)" || return 1
    else
        return 1
    fi
    printf '%s' "$v"
}

prune_downloads() {
    local rel="$DL_PUBLIC/releases" j="$DL_PUBLIC/latest.json"
    log "== downloads: $rel (keep latest.json version + previous) =="
    [ -d "$rel" ] || { warn "$rel missing; skipping"; return 0; }
    [ -f "$j" ] || die "$j missing; aborting downloads retention"
    local latest
    latest="$(read_latest_version "$j")" || die "cannot parse a .version from $j (need jq or python3); aborting"
    [[ "$latest" =~ $SEMVER_RE ]] || die "latest.json version '$latest' is not vX.Y.Z; aborting"
    [ -d "$rel/$latest" ] || die "latest.json names $latest but $rel/$latest does not exist; aborting"
    [ -n "$(find "$rel/$latest" -type f -print -quit)" ] || die "$rel/$latest is empty; aborting"

    local d v dirs=()
    for d in "$rel"/*/; do
        [ -d "$d" ] || continue
        v="$(basename "$d")"
        if [[ "$v" =~ $SEMVER_RE ]]; then dirs+=("$v"); else warn "ignoring non-semver dir $v"; fi
    done
    local sorted=() line
    while IFS= read -r line; do sorted+=("$line"); done < <(printf '%s\n' ${dirs[@]+"${dirs[@]}"} | sort_versions_desc)
    log "  latest.json = $latest; release dirs (newest first): ${sorted[*]}"

    # previous = highest version strictly below latest. Anything NEWER than
    # latest.json is left alone (an upload in flight publishes latest.json last).
    local latest_key prev="" key
    latest_key="$(semver_key "$latest")"
    for v in "${sorted[@]}"; do
        key="$(semver_key "$v")"
        if [[ "$key" < "$latest_key" ]]; then prev="$v"; break; fi
    done
    if [ -z "$prev" ]; then
        log "  no previous version below $latest; nothing to delete (keeping the rollback slot empty)"
        return 0
    fi
    log "  keeping $latest (current) and $prev (previous)"
    for v in "${sorted[@]}"; do
        key="$(semver_key "$v")"
        if [[ "$key" < "$(semver_key "$prev")" ]]; then
            [ "$v" = "$latest" ] && die "BUG: would delete the latest.json version"
            safe_rm "$rel/$v"
        elif [[ "$key" > "$latest_key" ]]; then
            log "  leaving newer-than-latest dir $v (upload in flight or unpublished)"
        fi
    done

    # Warn-only: older releases/<tag>/updater/ dirs are byte-copies of
    # public/updater/ (before build.yml stopped uploading them, the publish step
    # rsynced dist/ -- which contained updater/ -- AND dist/updater/). New
    # releases no longer have one; this only reports legacy ones.
    local sub f1 f2 rf
    for v in "$latest" "$prev"; do
        sub="$rel/$v/updater"
        [ -d "$sub" ] && [ -d "$DL_PUBLIC/updater" ] || continue
        f1="$(cd "$sub" && find . -type f -print -quit)"
        [ -n "$f1" ] || continue
        f2="$DL_PUBLIC/updater/${f1#./}"
        rf="$sub/${f1#./}"
        if [ -f "$f2" ] && ! [ "$rf" -ef "$f2" ] && cmp -s "$rf" "$f2"; then
            warn "$sub duplicates $DL_PUBLIC/updater ($(human "$(size_kb "$sub")") could be hardlinked; NOT acting)"
        fi
    done
}

# --- part: cargo debug target ----------------------------------------------

prune_target() {
    local t="$SERVER_ROOT/src/target/debug"
    log "== target: $t (target/release is kept) =="
    [ -d "$t" ] || { log "  no target/debug; nothing to do"; return 0; }
    safe_rm "$t"
}

# --- part: docker -----------------------------------------------------------

prune_docker() {
    log "== docker: dangling images + build cache only =="
    command -v docker >/dev/null 2>&1 || { warn "docker not installed; skipping"; return 0; }
    docker info >/dev/null 2>&1 || { warn "cannot talk to docker (not in the docker group?); skipping"; return 0; }
    # NEVER use `docker image prune -a` / `docker system prune -a`: that removes
    # every image not used by a RUNNING container, which includes the
    # intentionally-kept tagged monitoring images (prom/prometheus,
    # grafana/grafana, prom/alertmanager, node-exporter, ...) and app images of
    # stopped stacks. `docker image prune -f` (no -a) removes dangling
    # (<none>) images only. Volumes are never pruned either.
    if [ "$APPLY" -eq 1 ]; then
        docker builder prune -af 2>&1 | tail -1 | while IFS= read -r l; do log "  builder prune: $l"; done
        docker image prune -f 2>&1 | tail -1 | while IFS= read -r l; do log "  image prune: $l"; done
    else
        log "  would run: docker builder prune -af"
        log "  would run: docker image prune -f   (dangling only)"
        docker system df 2>&1 | while IFS= read -r l; do log "  $l"; done
        docker images -f dangling=true --format '  dangling: {{.ID}} {{.Size}}' 2>&1 | while IFS= read -r l; do log "$l"; done
    fi
}

# --- main -------------------------------------------------------------------

log "starting (parts: $ONLY${INCOMING:+, incoming $INCOMING})"
report_usage before
want releases && prune_releases
want downloads && prune_downloads
want target && prune_target
want docker && prune_docker
if [ "$APPLY" -eq 1 ]; then
    report_usage after
    log "done; freed about $(human "$FREED_KB") (excluding docker)"
else
    log "DRY RUN: would free about $(human "$FREED_KB") (excluding docker). Re-run with --apply to delete."
fi
