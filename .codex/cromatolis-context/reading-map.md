# Mapa de lectura — Cromatolis

Este es el orden de consulta de Codex; los enlaces relativos son deliberados
para no copiar contenido privado al repo público.

## Siempre primero

1. [`AGENTS.md`](../../AGENTS.md) — arquitectura, worktrees, LFS y política.
2. [`cromatolis-cartography`](../../.agents/skills/cromatolis-cartography/SKILL.md)
   — decidir si el problema pertenece al pipeline autoral de Cromatolis.
3. `docs/design/backlog/new-horizon.md` y
   `docs/design/backlog/cromatolis-open-world.md` — estado vivo y dependencias.

## Migración y arquitectura

- `docs/design/specs/2026-09-09-cromatolis-open-world-migration.md`
- `docs/design/plans/2026-09-10-cromatolis-new-horizon-migration-handoff.md`
- `docs/design/references/cromatolis-cartography/00-overview-and-pipeline.md`
- `docs/design/references/cromatolis-cartography/08-tooling-and-verification-workflow.md`

Consultar la referencia temática antes de cambiar una capa concreta: `01`
(coordenadas), `02` (altitud), `03` (agua), `04` (biomas), `05` (pendientes),
`06` (predicados de sitios), `07` (prioridad autoral/procedural) o `09`
(rutas, puentes y cuevas).

## Ciudadela Flotante — COW-8

- `docs/design/specs/2026-09-11-cow8-aerial-citadel-design.md`
- `docs/design/plans/2026-09-11-cow8-aerial-citadel-plan.md`
- `docs/design/tasks/147-cow8-aerial-citadel-tasks.md`

Las fuentes anteriores a la migración se consultan solo como evidencia:

- `xindeler-open-world/worlds/cromatolis/docs/*aerial-citadel*`
- `xindeler-old` en su commit de laboratorio consolidado.

No se hace un port ciego de ninguno. La implementación real de New Horizon y
los artefactos COW-8 anteriores determinan el punto de partida.
