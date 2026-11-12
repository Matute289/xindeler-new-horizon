# Deploy

> 🇦🇷 Versión en castellano: [README.md](README.md)

## VPS layout

```
/opt/xindeler-server/
├── src/                              # git checkout, updated by deploy.sh
├── xindeler-server-cli                # the running binary
├── xindeler-server-cli.previous       # the previous one, kept for rollback
├── portrait_gen                      # headless portrait renderer (NH-83), spawned per request
├── .env                              # secrets, not versioned
└── userdata/
    ├── server/saves/
    │   ├── db.sqlite                 # SQLite in WAL mode (characters, pacts, etc.)
    │   └── db-pre-deploy-*.sqlite    # automatic backups, one per deploy
    ├── server/rtsim/
    │   ├── data.dat                  # the simulated-world save (rtsim)
    │   └── data-pre-deploy-*.dat     # automatic backups, one per deploy
    └── server-cli/settings.ron       # `xindeler-server-cli` config (web port, etc.)
```

Runs under systemd as `xindeler-server-cli.service`, **not Docker**. Runs as user
`mgrinberg`, with `ProtectSystem=strict` and only `/opt/xindeler-server/userdata` writable.

## Deploying

```bash
ssh greenmountain.dev
bash /opt/xindeler-server/src/deploy/deploy.sh          # latest development
bash /opt/xindeler-server/src/deploy/deploy.sh v0.19.0  # a specific tag/ref
```

The script backs up the database and the rtsim save, pulls the code (or checks out the ref
passed as an argument, if given — leaves the checkout in detached HEAD), builds (nightly,
pinned by `rust-toolchain` — no `+toolchain` needed, plain `cargo` resolves it), keeps the
previous binary, restarts, and health-checks against `http://127.0.0.1:14005/health`. If the
new binary doesn't answer within 60 seconds, it automatically restores the previous one and
exits with an error.

**The build takes ~10-30 minutes** on the VPS (2 vCPU) — run it from a session that won't
drop, or under `nohup`/`tmux`.

It also builds and installs `portrait_gen` (the headless character-portrait renderer, NH-83) —
under the `dev` profile (no LTO), not `--release`: it lives in the `voxygen` crate, whose
release profile uses full LTO, which does not fit this VPS's 3.8 GB of RAM. No previous-binary
or dedicated rollback: it's spawned per request rather than run as a service, so a broken build
simply fails closed (`PortraitService` already treats any unexpected exit as `Failed` and serves
no portrait, without affecting the rest of the server).

## Tagged releases

`build-release.sh` (server-side, `/srv/git-lfs/scripts/`, not versioned in this repo) is what
`release.yml` triggers on every `v*` tag push. It's a thin wrapper around this same
`deploy.sh` — passes the tag as the ref, then packages the resulting binary into
`/srv/git-lfs/releases/`. It doesn't duplicate the build/install/health-check/rollback logic;
all of that lives here, in one reviewable place.

## Migrations

Migrations (`server/src/migrations/*.sql`) run automatically at startup via `refinery`
(`server/src/persistence/mod.rs`). Nothing to run by hand — the new binary applies them the
first time it starts against the existing database. That's why the `db.sqlite` backup happens
before the new binary is installed, not after.

## Manual rollback

If the script couldn't roll back on its own (it says `service is DOWN`):

```bash
cp /opt/xindeler-server/xindeler-server-cli.previous /opt/xindeler-server/xindeler-server-cli
sudo systemctl restart xindeler-server-cli.service
```

Only the binary changes — the database and rtsim save are untouched. If a migration also ran
and the database needs to go back too, restore the most recent
`db-pre-deploy-<timestamp>.sqlite` (+ `-wal`/`-shm`) instead of `db.sqlite`.

## See also

- `xindeler-auth/deploy/deploy.sh` — same pattern, same VPS, adapted here.
