# VPS retention: keep the deployed version + the previous one

Policy: only the deployed version and the one before it (rollback) are kept.
Older builds are deleted when a new version deploys, plus a monthly sweep.

`deploy/prune-old-builds.sh` is **dry-run by default**; `--apply` deletes.

| Part (`--only`) | What | Rule |
|---|---|---|
| `releases` | `/srv/git-lfs/releases/xindeler-server-v*.tar.gz` | newest 2 by semver (not mtime) |
| `downloads` | `/srv/xindeler/downloads/public/releases/<tag>/` | tag in `latest.json` + the next-lower semver; older deleted. Newer-than-latest and non-`vX.Y.Z` entries are never touched |
| `target` | `/opt/xindeler-server/src/target/debug` | removed (`target/release` kept; the next deploy rebuilds `portrait_gen` there anyway) |
| `docker` | dangling images + build cache | `docker builder prune -af` and `docker image prune -f` only |

Safety: takes an `flock`; refuses to run while `cargo`/`rustc`/`deploy.sh`/
`build-release.sh`/`docker build`/a downloads rsync is running (exit 3);
aborts without deleting if `latest.json` does not parse or its dir is missing;
all deletion goes through one `safe_rm` that refuses anything outside the
allowed roots, `xindeler-server-cli`, `.previous`, `portrait_gen`, and
`/srv/git-lfs/repos` (the single copy of the LFS binaries). Never `docker ... -a`
(it would delete the intentionally kept prom/grafana/alertmanager images).
Volumes are never pruned. Logs: `journalctl -t xindeler-prune`.

## Automatic hooks

- **Server deploy** (`deploy.sh`): after the health check passes, runs
  `prune-old-builds.sh --apply --only releases --from-deploy [--incoming <tag>]`,
  failure ignored. `build-release.sh` packs the new tarball *after* `deploy.sh`
  returns, hence `--incoming`: the new tag counts as one of the two kept, so the
  end state is exactly new + previous.
- **Client release** (`build.yml`, publish job): a last `continue-on-error` step runs
  `--only downloads` after `latest.json` points at the new tag. It is safe because
  the script re-validates `latest.json` itself and keeps latest + previous.
  If the VPS checkout predates the script the step just skips.
- **Monthly timer**: everything (`target`, `docker`, and a safety net for the two above).

## Install the timer (NOT done by the PR; needs sudo)

```bash
# on the VPS, after this is merged and /opt/xindeler-server/src has the script
cd /opt/xindeler-server/src/deploy
sudo cp xindeler-prune.service xindeler-prune.timer /etc/systemd/system/
sudo systemctl daemon-reload
sudo systemctl enable --now xindeler-prune.timer
systemctl list-timers xindeler-prune.timer
# first run by hand (dry run, then real):
bash prune-old-builds.sh
sudo systemctl start xindeler-prune.service && journalctl -t xindeler-prune -n 50
```

The service runs as `mgrinberg` (owner of the files, member of `docker`; no sudo).

## Known / not handled

- `public/releases/<tag>/updater/` (and the five `*-updater.zip` files next to the
  manual packages) were byte copies of `public/updater/` (~4 GB per release). Fixed in
  `build.yml`: the updater tree is generated outside `dist/` and the release rsync
  excludes `/updater/` and `*-updater.zip`, so releases published after that change
  no longer carry them. Releases published before it (v0.26.1 at the time of writing)
  keep their copies until retention deletes them; the script still only warns.
  To reclaim one by hand: `rm -rf public/releases/<tag>/updater public/releases/<tag>/*-updater.zip`
  (nothing reads them; `manifest.json`, `latest.json` and the rollback packages are untouched).
- `target/release` (~6.6 GB incremental/deps cache) is kept on purpose: deleting it
  makes every deploy a from-scratch ~30 min build. Docker volumes (~2.2 GB reclaimable,
  they hold DB data) and unused tagged images are never touched.
- `updater-releases/` (old updater test builds) is out of scope.

## Tests

`bash deploy/test-prune-old-builds.sh` (fixture dirs in a temp dir; no VPS/docker needed).
