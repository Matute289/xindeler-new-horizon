# Deploy

> 🇬🇧 English version: [README.en.md](README.en.md)

## Cómo está armado el VPS

```
/opt/xindeler-server/
├── src/                              # checkout de git, lo actualiza deploy.sh
├── xindeler-server-cli                # el binario en ejecución
├── xindeler-server-cli.previous       # el anterior, guardado para rollback
├── portrait_gen                      # renderer headless de retratos (NH-83), spawneado por pedido
├── .env                              # secretos, no versionado
└── userdata/
    ├── server/saves/
    │   ├── db.sqlite                 # SQLite en modo WAL (personajes, pactos, etc.)
    │   └── db-pre-deploy-*.sqlite    # backups automáticos previos a cada deploy
    ├── server/rtsim/
    │   ├── data.dat                  # el save del mundo simulado (rtsim)
    │   └── data-pre-deploy-*.dat     # backups automáticos previos a cada deploy
    └── server-cli/settings.ron       # config del `xindeler-server-cli` (puerto web, etc.)
```

El servicio corre bajo systemd como `xindeler-server-cli.service`, **no en Docker**. Usa el
usuario `mgrinberg`, con `ProtectSystem=strict` y solo `/opt/xindeler-server/userdata`
escribible.

## Desplegar

```bash
ssh greenmountain.dev
bash /opt/xindeler-server/src/deploy/deploy.sh          # último development
bash /opt/xindeler-server/src/deploy/deploy.sh v0.19.0  # un tag/ref específico
```

El script hace backup de la base y del save de rtsim, actualiza el código (o hace checkout
del ref pasado como argumento, si se pasa uno — queda en detached HEAD), compila (nightly,
pinned por `rust-toolchain` — no hace falta `+toolchain`, `cargo` lo resuelve solo), guarda
el binario anterior, reinicia y verifica salud contra `http://127.0.0.1:14005/health`. Si el
binario nuevo no responde en 60 segundos, restaura el anterior automáticamente y sale con
error.

**La build tarda ~10-30 minutos** en la VPS (2 vCPU) — correrlo en una sesión que no se vaya a
cortar, o con `nohup`/`tmux`.

También compila e instala `portrait_gen` (el binario headless que renderiza retratos de
personaje, NH-83) — en perfil `dev` (sin LTO), no `--release`: vive en el crate `voxygen`, cuyo
perfil de release usa LTO completo, que no entra en los 3.8 GB de RAM de esta VPS. Sin binario
anterior ni rollback dedicado: se spawnea por pedido, no corre como servicio, así que una build
rota simplemente falla cerrado (`PortraitService` ya trata cualquier salida inesperada como
`Failed` y no sirve retrato, sin afectar al resto del servidor).

## Releases taggeados

`build-release.sh` (server-side, `/srv/git-lfs/scripts/`, no versionado en este repo) es lo
que dispara `release.yml` en cada push de un tag `v*`. Es un wrapper fino sobre este mismo
`deploy.sh` — le pasa el tag como ref, y después empaqueta el binario resultante en
`/srv/git-lfs/releases/`. No duplica la lógica de build/instalación/health-check/rollback;
toda esa lógica vive acá, en un solo lugar revisable.

## Migraciones

Las migraciones (`server/src/migrations/*.sql`) corren automáticamente al arrancar, vía
`refinery` (`server/src/persistence/mod.rs`). No hace falta correrlas a mano — el binario
nuevo las aplica solo la primera vez que levanta contra la base existente. Por eso el backup
de `db.sqlite` es antes de instalar el binario nuevo, no después.

## Rollback manual

Si el script no pudo hacer rollback solo (dice `service is DOWN`):

```bash
cp /opt/xindeler-server/xindeler-server-cli.previous /opt/xindeler-server/xindeler-server-cli
sudo systemctl restart xindeler-server-cli.service
```

El binario anterior queda intacto en `.sqlite`/`.dat` — solo el binario cambia. Si además una
migración corrió y necesitás volver la base atrás, restaurá el backup
`db-pre-deploy-<timestamp>.sqlite` (+ `-wal`/`-shm`) más reciente en vez de `db.sqlite`.

## Ver también

- `xindeler-auth/deploy/deploy.sh` — mismo patrón, mismo VPS, adaptado acá.
