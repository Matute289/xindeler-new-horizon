# Reglas operativas de Cromatolis

## Repositorios y datos

1. Antes de leer o escribir `docs/design/`, ejecutar allí `git status` y
   `git pull --ff-only`. Es un repositorio privado separado y compartido.
2. `xindeler-open-world` produce datos; New Horizon consume sus exportaciones.
   No editar desde este repo los masters, intermedios o contratos autorales de
   Open World.
3. Los masters L16 están en
   `~/MyXindeler/OpenWorld/Cromatolis/l16-v10/`. Nunca sobrescribir una revisión
   existente: crear un nuevo `_v<N+1>.tif`, reconstruir el pipeline y muestrear
   el `SimChunk` real.
4. `assets/world/map/cromatolis_v0*` son productos de exportación. No editar a
   mano `.bin` ni `.f32le`.

## Git y LFS

1. Los binarios LFS de New Horizon van exclusivamente al store SSH del VPS,
   configurado por `.lfsconfig`; GitHub recibe punteros LFS, nunca blobs ni
   GitHub LFS.
2. Nunca hacer push directo ni mergear `main` o `development`. Para Cromatolis,
   el trabajo derivado vuelve primero por PR a
   `new-horizon/open-world-cromatolis`.
3. Antes de editar, revisar `git status`. El worktree puede contener trabajo de
   otra sesión; se preserva y se trabaja alrededor de él.

## Disciplina técnica

1. El pipeline de Cromatolis es numérico, no una revisión visual de TIFFs de
   32768×24576. Consultar `cromatolis-cartography` y verificar contra el motor
   regenerado.
2. Mantener la abstracción por región: Cromatolis V1 puede ser la única región
   abierta, pero no convertirlo en el supuesto de que todo el mundo cargado es
   Cromatolis.
3. Una capa no está terminada porque exista su raster/RON: necesita consumidor
   de runtime, resultado visible/físico y prueba.
4. Para la Ciudadela, conservar su alcance probado: isla, castillo, 24 torres,
   48 cañones, escudos, animación y disparos de práctica inofensivos. No sumar
   ciudad superficial, agua, cuevas, daño, selección de objetivos ni fuego
   automático sin una fila de backlog nueva.

## Coordinación futura

Cuando exista `.communication-room/`, será el canal asíncrono con Claude. No
crear ni editar ese directorio hasta que Matías defina su contrato y formato.
