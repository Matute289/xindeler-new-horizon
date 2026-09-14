# Contexto operativo de Codex — Cromatolis

Este directorio es el índice local de Codex para el programa Cromatolis. No es
una segunda fuente de diseño: los contratos, specs, planes y task boards
canónicos viven en el repositorio privado `docs/design/`. Tampoco reemplaza
archivos de Claude; los enlaza para que ambos agentes trabajen sobre la misma
evidencia.

## Punto de partida actual

- Worktree de trabajo: `xindeler-new-horizon-cromatolis`.
- Rama de integración de Cromatolis: `new-horizon/open-world-cromatolis`.
- Nuevo trabajo: crear una rama/worktree aislado desde esa rama y volver por
  PR hacia ella. No tocar cambios locales ajenos que ya existan en el
  worktree.
- `xindeler-new-horizon` es el motor, cliente y servidor públicos;
  `docs/design/` es el repo privado de diseño anidado.
- `xindeler-open-world` es la fuente canónica de datos autorales. `xindeler-old`
  es una referencia de laboratorio congelada, nunca un destino de trabajo
  nuevo.

## Skills y agentes existentes

No se mantienen copias alternativas aquí. Las rutas fuente son:

- `.agents/skills/cromatolis-cartography/SKILL.md` — máscaras, capas y el
  pipeline real de Cromatolis.
- `.agents/skills/xindeler-worldgen/SKILL.md` — worldgen que no sea un caso de
  datos autorales de Cromatolis.
- `.agents/skills/xindeler-worldmap/SKILL.md` — diseño de mapa/lore; no usarlo
  para editar los assets `cromatolis_v0*`.
- `.agents/skills/xindeler-voxel-authoring/SKILL.md` y
  `.agents/skills/xindeler-particle-authoring/SKILL.md` — assets y VFX de la
  Ciudadela.
- `.codex/agents/cromatolis-terrain-engineer.toml` — especialista de datos de
  terreno. Su par legible para Claude vive en
  `.claude/agents/cromatolis-terrain-engineer.md`.

Las copias de Claude (`.claude/skills/`) y las de Codex (`.agents/skills/`)
son espejos validados por `scripts/validate-agent-config.py`; no editar una
sin aplicar el mecanismo de mirror del repositorio.

## Estado que importa para la próxima sesión

`COW-8` (Ciudadela Aérea) está implementado y cerrado en el backlog: las cinco
fases llegaron por los PRs #298, #299, #301 y #303. Quedan solo las pruebas
manuales en cliente `T14` y `T24` del task board: inspección visual/de
estabilidad de la Ciudadela y disparos de práctica de ambos cañones. No asumir
que haya una fase de código pendiente sin releer el board.

Ver [reading-map.md](reading-map.md) para el orden de lectura y
[operating-rules.md](operating-rules.md) para las restricciones que no se
pueden violar.
