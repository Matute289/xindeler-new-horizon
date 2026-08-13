command-help-template = { $usage } { $description }
command-help-list =
    { $client-commands }
    { $server-commands }

    Además, puedes utilizar los siguientes atajos
    { $additional-shortcuts }
command-adminify-desc = Otorga temporalmente a un jugador el rol de administrador restringido o elimina el actual (si aún no se ha otorgado)
command-airship-desc = Genera un dirigible
command-alias-desc = Cambia tu alias
command-area_add-desc = Añade una nueva área de construcción
command-area_list-desc = Lista todas las áreas de construcción
command-area_remove-desc = Elimina el área de construcción especificada
command-aura-desc = Crea un aura
command-body-desc = Cambia tu cuerpo a una especie diferente
command-set_body_type-desc = Selecciona tu tipo de cuerpo, Femenino o Masculino.
command-set_body_type-not_found =
    Ese no es un tipo de cuerpo válido.
    Prueba uno de estos:
    { $options }
command-set_body_type-no_body = No se pudo establecer el tipo de cuerpo ya que el objetivo no tiene un cuerpo.
command-set_body_type-not_character = Solo puede establecer permanentemente un tipo de cuerpo si el objetivo es un jugador conectado como personaje.
command-buff-desc = Aplica un potenciador al jugador
command-build-desc = Activa y desactiva el modo de construcción
command-ban-desc = Bloquea a un jugador con un determinado nombre de usuario, por un periodo determinado (si se proporciona). Indique "true for overwrite" para modificar un bloqueo existente.
command-ban-ip-desc = Bloquea a un determinado jugador, por un periodo de tiempo determinado (si es provisto). A diferencia de un bloqueo normal, este también bloquea la dirección IP asociada con este usuario. Indique "true for overwrite" para modificar un bloqueo existente.
command-banish-desc = (Administrador) Destierra temporalmente al objetivo por N segundos (herramienta de prueba para el destierro real de 1 a 7 días)
command-battlemode-desc =
    Configura tu modo de batalla a:
    + pvp (jugador vs jugador)
    + pve (jugador vs entorno).
    Si se usa sin argumentos, mostrará el modo de batalla actual.
command-battlemode_force-desc = Cambia tu estado de combate sin ninguna comprobación
command-campfire-desc = Crea una hoguera
command-clear_persisted_terrain-desc = Limpia terreno cercano que sea persistente
command-create_location-desc = Crea una ubicación en la posición actual
command-death_effect-dest = Añade un efecto al morir en la entidad objetivo
command-debug_column-desc = Imprime información de depuración sobre una columna
command-debug_ways-desc = Imprime información de depuración sobre las formas de una columna
command-delete_location-desc = Elimina una ubicación
command-destroy_tethers-desc = Destruye todos los lazos conectados a ti
command-disconnect_all_players-desc = Desconecta a todos los jugadores del servidor
command-dismount-desc = Desmonta si estás montando, o desmonta cualquier cosa que te monte
command-dropall-desc = Deja caer todos tus objetos al suelo
command-dummy-desc = Genera un muñeco de entrenamiento
command-explosion-desc = Explota el suelo a tu alrededor
command-faction-desc = Envía mensajes a tu facción
command-give_item-desc = Te da algunos objetos. Para ejemplos o auto completar, usa Tab.
command-gizmos-desc = Administra las subscripciones gizmo.
command-gizmos_range-desc = Cambia el rango de las suscripciones gizmo.
command-goto-desc = Teletransporta a una posición
command-goto-rand = Teletransporta a una posición aleatoria
command-group-desc = Envía mensajes a tu grupo
command-group_invite-desc = Invita a un jugador a unirse al grupo
command-group_kick-desc = Remueve a un jugador del grupo
command-group_leave-desc = Abandona el grupo actual
command-group_promote-desc = Promueve un jugador a líder de grupo
command-health-desc = Establece tu salud actual
command-into_npc-desc = Te convierte a ti en un NPC. Ten cuidado!
command-join_faction-desc = Unirse/abandonar la facción especificada
command-jump-desc = Desplaza tu posición actual
command-kick-desc = Expulsa a un jugador con un nombre de usuario indicado
command-kill-desc = Suicidarte
command-kill_npcs-desc = Mata a los NPCs
command-kit-desc = Coloca un conjunto de objetos en tu inventario.
command-lantern-desc = Cambia la potencia y color de tu linterna
command-light-desc = Crea una entidad con luz
command-lightning-desc = Caída de un rayo en la posición actual
command-location-desc = Teletransportarse a un lugar
command-make_block-desc = Crea un bloque en tu ubicación con un color
command-make_npc-desc =
    Genera una entidad a partir de la configuración cercana.
    Para ver un ejemplo o autocompletar, pulsa Tab.
command-spawned-airship = Ha generado un dirigible
command-make_sprite-desc = Crea un sprite en tu ubicación; para definir los atributos del sprite, utiliza la sintaxis de Ron para un StructureSprite.
command-make_volume-desc = Crear un volumen (experimental)
command-motd-desc = Ver la descripción del servidor
command-mount-desc = Montar una entidad
command-object-desc = Crear un objeto
command-outcome-desc = Crear un resultado
command-pact-desc = (Admin) Gestiona el pacto de un Brujo: 'bind <patrón> [objetivo]' lo vincula a un patrón, 'sever [objetivo]' rompe el pacto (deshabilitando su magia), 'status [objetivo]' informa su estado
command-permit_build-desc = Ofrece al jugador un espacio delimitado en el que puede construir
command-players-desc = Lista los jugadores conectados en este momento
command-poise-desc = Establece tu equilibrio actual
command-portal-desc = Crea un portal
command-region-desc = Envía mensajes a todes en tu región del mundo
command-reload_chunks-desc = Vuelve a cargar los fragmentos cargados en el servidor
command-remove_lights-desc = Elimina todas las luces generadas por los jugadores
command-repair_equipment-desc = Repara todos los objetos equipados
command-reset_recipes-desc = Restablece tu libro de recetas
command-respawn-desc = Teletranspórtate a tu punto de ruta
command-revoke_build-desc = Revoca el permiso de construcción de área del jugador
command-revoke_build_all-desc = Revoca todos los permisos de área de construcción del jugador
command-safezone-desc = Crea una zona segura
command-say-desc = Envía mensajes a todos los que estén a un grito de distancia
command-scale-desc = Ajusta el tamaño de tu personaje
command-server_physics-desc = Activar/desactivar las físicas de autoridad del servidor para una cuenta
command-set_motd-desc = Establece la descripción del servidor
command-set-waypoint-desc = Establece tu punto de referencia en tu ubicación actual.
command-ship-desc = Genera una nave
command-site-desc = Teletransportarse a un sitio
command-skill_point-desc = Te das puntos de habilidad para un árbol de habilidades concreto
command-skill_preset-desc = Otorga a tu personaje las habilidades deseadas.
command-spawn-desc = Crear una entidad de prueba
command-spot-desc = Busca y teletranspórtate al lugar más cercano de un tipo determinado.
command-sudo-desc = Ejecuta el comando como si fueras otra entidad
command-tell-desc = Enviar un mensaje a otro jugador
command-tether-desc = Vincula a otra entidad a ti mismo
command-time-desc = Establece la hora del día
command-time_scale-desc = Establecer la escala del tiempo delta
command-tp-desc = Teletransportarse a otra entidad
command-rtsim_chunk-desc = Mostrar información sobre el fragmento actual de rtsim
command-rtsim_info-desc = Mostrar información sobre un rtsim de NPC
command-rtsim_npc-desc = Enumera los rtsim de NPC que se ajusten a una consulta determinada (ejemplo: simulado, comerciante) ordenados por distancia
command-rtsim_purge-desc = Borrar los datos de rtsim en el próximo inicio
command-rtsim_tp-desc = Teletransportarse a un rtsim de npc
command-unban-desc = Elimina el bloqueo del nombre de usuario indicado. Si hay un bloqueo de IP asociado, también se eliminará.
command-unban-ip-desc = Elimina únicamente el bloqueo de IP asociado a ese nombre de usuario.
command-version-desc = Indica la versión del servidor
command-weather_zone-desc = Crear una zona climática
command-whitelist-desc = Añade o elimina un nombre de usuario a la lista blanca
command-wiring-desc = Crear elemento de cableado
command-world-desc = Envia mensajes a todos los usuarios del servidor
command-wiki-desc = Abre la wiki o busca un tema
command-reset_tutorial-desc = Restablecer el tutorial del juego a su estado inicial
command-reset_tutorial-success = Restablecer el estado del tutorial.
command-naga-desc = Activar o desactivar el uso de Naga en el procesamiento inicial del sombreador (no se guarda)
players-list-header =
    { $count ->
        [1]
            { $count } jugador en línea
            { $player_list }
       *[other]
            { $count } jugadores en líne
            { $player_list }
    }
command-clear-desc = Borra todos los mensajes del chat. Afecta a todas las pestañas del chat.
command-experimental_shader-desc = Activa o desactiva un sombreador experimental.
command-help-desc = Mostrar información sobre los comandos
command-mute-desc = Silencia los mensajes de chat de un jugador.
command-unmute-desc = Desactiva el silencio de un jugador que se había silenciado con el comando «mute».
command-waypoint-desc = Mostrar la ubicación del punto de ruta actual
command-preprocess-target-error = Se espera { $expected_list } después de '@' encontrado { $target }
command-preprocess-not-looking-at-valid-target = No se está apuntando a un objetivo válido
command-preprocess-not-selected-valid-target = No se ha seleccionado un objetivo válido
command-preprocess-not-valid-viewpoint-entity = No se está visualizando desde una entidad de punto de vista válida
command-preprocess-not-riding-valid-entity = No se está montando una entidad válida
command-preprocess-not-valid-rider = No hay ningún jinete válido
command-preprocess-no-player-entity = No hay entidad de jugador
command-invalid-command-message =
    No se encontró el comando { $invalid-command }.
    ¿Quizás te refieres a alguno de los siguientes?
    { $most-similar-command }
    { $commands-with-same-prefix }

    Escribe /help para ver una lista de todos los comandos.
command-mute-cannot-mute-self = No puedes silenciarte
command-mute-success = Se ha silenciado correctamente a { $player }
command-mute-no-player-found = No se ha encontrado ningún jugador llamado { $player }
command-mute-already-muted = { $player } ya está silenciado
command-mute-no-player-specified = Debes especificar un jugador
command-unmute-cannot-unmute-self = No puedes quitarte un silenciado a ti
command-unmute-success = Se han reactivado mensajes de { $player }
command-unmute-no-muted-player-found = No se ha encontrado ningún jugador silenciado llamado { $player }
command-unmute-no-player-specified = Debes seleccionar un jugador para silenciarlo
command-shader-backend = Backend de sombreado actual: { $shader-backend }
command-experimental-shaders-list = { $shader-list }
command-experimental-shaders-not-found = No hay sombreadores experimentales
command-experimental-shaders-enabled = Habilitado { $shader }
command-experimental-shaders-disabled = Deshabilitado { $shader }
command-experimental-shaders-not-a-shader = { $shader } no es un sombreador experimental; utiliza este comando con cualquier argumento para ver una lista completa.
command-experimental-shaders-not-valid = Debes especificar un sombreador experimental válido; para obtener una lista de sombreadores experimentales, utiliza este comando sin ningún argumento.
command-no-permission = No tienes permiso para usar «/{ $command_name }»
command-position-unavailable = No se puede obtener la posición de { $target }
command-player-role-unavailable = No se pueden obtener los roles de administrador para { $target }
command-uid-unavailable = No se puede obtener el UID de { $target }
command-area-not-found = No se ha encontrado el área llamada «{ $area }»
command-player-not-found = ¡No se ha encontrado el jugador «{ $player }»!
command-player-uuid-not-found = ¡No se ha encontrado el jugador con el UUID «{ $uuid }»!
command-username-uuid-unavailable = No se pudo determinar el UUID para el nombre de usuario { $username }
command-uuid-username-unavailable = No se pudo determinar el nombre de usuario para el UUID  { $uuid }
command-no-sudo = Es grosero hacerse pasar por otra persona
command-entity-dead = ¡La entidad «{ $entity }» está muerta!
command-error-write-settings =
    No se pudo guardar el archivo de configuración en el disco, pero sí en la memoria.
    Error (almacenamiento): { $error }
    Éxito (memoria): { $message }
command-error-while-evaluating-request = Se produjo un error al validar la solicitud: { $error }
command-give-inventory-full =
    El inventario del jugador está lleno. Se entregó { $given ->
        [1] solo uno
       *[other] { $given }
    } de { $total } objetos.
command-give-inventory-success = Se han añadido { $total } x { $item } al inventario.
command-invalid-item = Elemento inválido: { $item }
command-invalid-block-kind = Tipo de bloque inválido: { $kind }
command-nof-entities-at-least = El número de entidades debe ser de al menos 1
command-nof-entities-less-than = El número de entidades debe ser inferior a 50
command-entity-load-failed = No se pudo cargar la configuración de la entidad: { $config }
command-spawned-entities-config = Se han generado { $n } entidades a partir de la configuración: { $config }
command-invalid-sprite = Tipo de sprite no válido: { $kind }
command-time-parse-too-large = { $n } es inválido; no puede tener más de 16 dígitos.
command-time-parse-negative = { $n } es inválido; no puede ser negativo.
command-time-backwards = { $t } es anterior a la hora actual; el tiempo no puede retroceder.
command-time-invalid = { $t } no es una hora válida.
command-time-current = Es { $t }
command-time-unknown = Hora desconocida
command-rtsim-purge-perms = Debes ser un administrador real (no solo un admin temporal) para purgar los datos de rtsim.
command-chunk-not-loaded = Bloque { $x }, { $y } no cargado
command-chunk-out-of-bounds = El fragmento { $x }, { $y } no se encuentra dentro de los límites del mapa
command-spawned-entity = Se ha creado la entidad con el ID: { $id }
command-spawned-dummy = Generado un maniquí de entrenamiento

command-adminify-already-has-no-role = ¡El jugador ya no tiene ningún rol!

command-adminify-already-has-role = ¡El jugador ya tiene ese rol!

command-adminify-assign-higher-than-own = No se puede asignar a alguien un rol temporal más alto que el rol permanente propio.

command-adminify-cannot-find-player = ¡No se puede encontrar la entidad del jugador!

command-adminify-reassign-to-above = No se puede reasignar un rol a nadie con tu rol o superior.

command-adminify-removed-role = Rol eliminado del jugador { $player }: { $role }

command-adminify-role-downgraded = Rol del jugador { $player } degradado a { $role }

command-adminify-role-upgraded = Rol del jugador { $player } ascendido a { $role }

command-aura-invalid-buff-parameters = Parámetros de potenciador inválidos para aura

command-aura-tiered-effect-unsupported = No se pueden generar auras de efecto por escalones de vida con este comando

command-aura-spawn = Se generó un nuevo aura adjunto a la entidad

command-aura-spawn-new-entity = Se generó un nuevo aura

command-ban-added = Se añadió { $player } a la lista de bloqueos con razón: { $reason }

command-ban-already-added = { $player } ya está en la lista de bloqueos

command-ban-ip-added = Se añadió { $player } a la lista de bloqueos normal y de IP con razón: { $reason }

command-ban-ip-queued = Se añadió { $player } a la lista de bloqueos normal y se puso en cola un bloqueo de IP con razón: { $reason }

command-battlemode-available-modes = Modos disponibles: pvp, pve

command-battlemode-cooldown = Período de enfriamiento activo. Intenta de nuevo en { $cooldown } segundos

command-battlemode-intown = ¡Necesitas estar en la ciudad para cambiar el modo de batalla!

command-battlemode-same = Se intentó establecer el mismo modo de batalla

command-battlemode-updated = Nuevo modo de batalla: { $battlemode }

command-buff-body-unknown = Especificación de cuerpo desconocida: { $spec }

command-buff-data = El argumento de potenciador '{ $buff }' requiere datos adicionales

command-buff-spec-invalid = Especificación de datos inválida: { $spec }

command-buff-unknown = Potenciador desconocido: { $buff }

command-cannot-send-message-hidden = No se pueden enviar mensajes como espectador oculto.

command-client-has-no-socketaddr = No se puede obtener la dirección de socket (conectado a través de conexión mpsc) para { $target }

command-death_effect-unknown = Efecto de muerte desconocido { $effect }.

command-destroyed-no-tethers = No estás conectado a ningún lazo

command-destroyed-tethers = ¡Se destruyeron todos los lazos! Ahora eres libre

command-disabled-by-settings = Comando deshabilitado en la configuración del servidor

command-disconnectall-confirm = Por favor, ejecuta el comando nuevamente con el segundo argumento "confirm" para confirmar que
  realmente quieres desconectar a todos los jugadores del servidor

command-dismounted = Desmontado

command-entity-has-no-client = El jugador no tiene componente de cliente: { $target }

command-experimental-shaders-not-supported = { $shader } no es compatible con esta compilación del juego

command-experimental-terrain-persistence-disabled = La persistencia de terreno experimental está deshabilitada

command-explosion-power-too-high = La potencia de la explosión no debe ser más de { $power }

command-explosion-power-too-low = La potencia de la explosión debe ser más de { $power }

command-faction-join = Por favor, únete a una facción con /join_faction

command-give_item_quality-desc = (Administrador) Date a ti mismo un objeto en un nivel de rareza elegido, para comparar la representación de niveles de rareza: item quality [num]

command-give_item_quality-success = Se añadieron { $total } x { $item } (calidad { $quality }) al inventario.

command-group_invite-invited-to-group = Se invitó a { $player } al grupo.

command-group_invite-invited-to-your-group = { $player } ha sido invitado a tu grupo.

command-group-join = Por favor, crea un grupo primero

command-into_npc-warning = ¡Espero que no lo estés abusando!

command-invalid-alignment = Alineamiento inválido: { $alignment }

command-invalid-skill-group = ¡{ $group } no es un grupo de habilidades!

command-inventory-cant-fit-item = No se puede ajustar el objeto al inventario

command-kick-higher-role = No se puede expulsar a jugadores con roles más altos que el tuyo.

command-kit-inventory-unavailable = No se pudo obtener el inventario

command-kit-not-enough-slots = El inventario no tiene suficientes espacios

command-lantern-adjusted-strength = Ajustaste la intensidad de la llama.

command-lantern-adjusted-strength-color = Ajustaste la intensidad y el color de la llama.

command-lantern-unequiped = Por favor, equipa una linterna primero

command-location-created = Se creó la ubicación '{ $location }'

command-location-deleted = Se eliminó la ubicación '{ $location }'

command-location-duplicate = La ubicación '{ $location }' ya existe, considera eliminarla primero

command-location-invalid = El nombre de ubicación '{ $location }' no es válido. Los nombres solo pueden contener ASCII minúscula e
  guiones bajos

command-location-not-found = La ubicación '{ $location }' no existe

command-locations-empty = No hay ubicaciones actualmente existentes

command-locations-list = Ubicaciones disponibles: { $locations }

command-make_party-desc = (Administrador) Genera 3 NPCs cerca de ti, cada uno con una clase y nivel dados, con un nombre/raza aleatorio y una alineación moral compatible con la tuya, luego agrúpalos contigo: class1 level1 class2 level2 class3 level3

command-make_test_char-desc = (Administrador) Configura un personaje de prueba de una sola vez: level [class] [kit]

command-message-group-missing = Estás usando el chat de grupo pero no perteneces a ningún grupo. Usa /world o
  /region para cambiar el chat.

command-no-buid-perms = No tienes permiso para construir.

command-no-dismount = No estás montando ni siendo montado

command-outcome-expected_body_arg = Se esperaba argumento de cuerpo

command-outcome-expected_entity_arg = Se esperaba argumento de entidad

command-outcome-expected_frontent_specifier = Se esperaba especificador de interfaz

command-outcome-expected_integer = Se esperaba número entero

command-outcome-expected_skill_group_kind = Se esperaba un SkillGroupKind válido en ron

command-outcome-expected_sprite_kind = Se esperaba SpriteKind

command-outcome-invalid_outcome = { $outcome } no es un resultado válido

command-outcome-variant_expected = Se esperaba variante de resultado

command-parse-duration-error = No se pudo analizar la duración: { $error }

command-permit-build-given = Ahora tienes permiso para construir en '{ $area }'

command-permit-build-granted = Se otorgó permiso para construir en '{ $area }'

command-player-info-unavailable = No se puede obtener la información del jugador para { $target }

command-reloaded-chunks = Se recargaron { $reloaded } fragmentos

command-repaired-inventory_items = Se repararon todos los objetos

command-repaired-items = Se repararon todos los objetos equipados

command-respawn-no-waypoint = No hay punto de ruta establecido

command-revoke-build = Se revocó el permiso para construir en '{ $area }'

command-revoke-build-all = Se han revocado todos tus permisos de construcción.

command-revoke-build-recv = Tu permiso para construir en '{ $area }' ha sido revocado

command-revoked-all-build = Todos los permisos de construcción revocados.

command-scale-set = Se estableció la escala a { $scale }

command-server-no-experimental-terrain-persistence = El servidor fue compilado sin persistencia de terreno habilitada

command-set_class-desc = Selección de clase única para personajes heredados: warrior, mage, cleric o rogue

command-set_class_level-desc = (Administrador) Establece el nivel de clase primaria o secundaria de un personaje multiclase, para pruebas

command-set_ethos-desc = (Administrador) Establece el alineamiento moral del objetivo: <good|neutral|evil> <lawful|neutral|chaotic>

command-set_level-desc = (Administrador) Establece el nivel de personaje del objetivo (1-60) para pruebas, sin molienda

command-multiclass-desc = (Administrador) Otorga una segunda clase al objetivo (tope de 2), para pruebas

command-trigger_slot-desc = (Administrador) Configura un slot de trigger: <slot 0-3> <índice del pool de habilidades> <health_below|damage_taken|energy_below> [umbral 0-1]

command-trigger_ready-desc = (Administrador) Fuerza un slot de trigger a listo, borrando su cooldown de tiempo real

command-set_motd-message-added = Se estableció el mensaje del servidor del día a { $message }

command-set_motd-message-not-set = Esta configuración regional no tenía ningún motd establecido

command-set_motd-message-removed = Se eliminó el mensaje del servidor del día

command-set-build-mode-off = Se desactivó el modo de construcción.

command-set-build-mode-on-persistent = Se activó el modo de construcción. La persistencia de terreno experimental está habilitada. El servidor intentará persistir los cambios, pero esto no está garantizado.

command-set-build-mode-on-unpersistent = Se activó el modo de construcción. Los cambios no se persistirán cuando se descargue un fragmento.

command-set-waypoint-result = ¡Punto de ruta establecido!

command-site-not-found = Sitio no encontrado

command-skillpreset-broken = La preconfiguración de habilidades está rota

command-skillpreset-load-error = Error al cargar preconfigurations

command-skillpreset-missing = La preconfiguración no existe: { $preset }

command-spawned-campfire = Se generó una hoguera

command-spawned-safezone = Se generó una zona segura

command-spot-spot_not_found = No se encontró ningún lugar de ese tipo en este mundo.

command-spot-world_feature = La característica `worldgen` tiene que estar habilitada para ejecutar este comando.

command-sudo-higher-role = No se puede sudo a jugadores con roles más altos que el tuyo.

command-sudo-no-permission-for-non-players = No tienes permiso para hacer sudo en no-jugadores.

command-tell-to-yourself = No puedes /tell a ti mismo.

command-time_scale-changed = Se estableció la escala de tiempo a { $scale }.

command-time_scale-current = La escala de tiempo actual es { $scale }.

command-transform-invalid-presence = No se puede transformar en la presencia actual

command-unban-already-unbanned = { $player } ya había sido desbloqueado.

command-unban-ip-successful = La IP bloqueada a través del usuario "{ $player }" fue desbloqueada exitosamente (este usuario seguirá siendo bloqueado)

command-unban-successful = { $player } fue desbloqueado exitosamente.

command-unimplemented-spawn-special = No está implementada la generación de entidades especiales

command-unknown = Comando desconocido

command-version-current = El servidor está ejecutando { $version }

command-volume-created = Se creó un volumen

command-volume-size-incorrect = El tamaño debe estar entre 1 y 127.

command-waypoint-error = No se pudo encontrar tu punto de ruta.

command-waypoint-result = Tu punto de ruta actual está en { $waypoint };

command-weather-valid-values = Los valores válidos son 'clear', 'rain', 'wind' y 'storm'.

command-whitelist-added = Se añadió a la lista blanca: { $username }

command-whitelist-already-added = ¡Ya está en la lista blanca: { $username }!

command-whitelist-permission-denied = Permiso denegado para eliminar usuario: { $username }

command-whitelist-removed = Se eliminó de la lista blanca: { $username }

command-whitelist-unlisted = No forma parte de la lista blanca: { $username }

command-you-dont-exist = No existes, por lo que no puedes usar este comando
