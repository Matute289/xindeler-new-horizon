## Regeneration

buff-heal = Sanar
    .desc = Restaura salud con el tiempo.
    .stat =
        { $duration ->
            [1] Restaura { $str_total } puntos de salud por { $duration } segundo.
           *[other] Restaura { $str_total } puntos de salud por { $duration } segundos.
        }

## Potion

buff-potion = Poción
    .desc = Bebiendo...

## Saturation

buff-saturation = Saturación
    .desc = Los consumibles te hacen recuperar salud con el paso del tiempo.

## Campfire


## Energy Regen

buff-energy_regen = Regeneración de energía
    .desc = Regeneración de energía más rápida.
    .stat =
        { $duration ->
            [1] Restaura { $str_total } de energía por { $duration } segundo.
           *[other] Restaura { $str_total } de energía por { $duration } segundos.
        }

## Health Increase

buff-increase_max_health = Aumento de salud
    .desc = Tu salud máxima es aumentada.
    .stat =
        { $duration ->
            [1]
                Aumenta la vida maxima
                en { $strength }.
                Durante { $duration } segundo.
           *[other]
                Aumenta la vida maxima
                 en { $strength }.
                 Durante { $duration } segundos.
        }

## Energy Increase

buff-increase_max_energy = Aumento de energía
    .desc = Tu energía máxima es incrementada.
    .stat =
        { $duration ->
            [1]
                Aumenta la energía máxima
                en { $strength }.
                Durante { $duration } segundo.
           *[other]
                Aumenta la energía máxima
                en { $strength }.
                Durante { $duration } segundos.
        }

## Invulnerability

buff-invulnerability = Invulnerabilidad
    .desc = No puedes ser herido por ningún ataque
    .stat =
        { $duration ->
            [1]
                Concede invulnerabilidad.
                Durante { $duration } segundo.
           *[other]
                Concede invulnerabilidad.
                Durante { $duration } segundos.
        }

## Protection Ward

buff-protectingward = Custodia
    .desc = Los ataques no te hacen daño.

## Frenzied

buff-frenzied = Frenesí
    .desc = Consigues una velocidad sobrehumana e ignoras las pequeñas heridas.

## Haste

buff-hastened = Prisa
    .desc = Te mueves y atacas más rápido.

## Bleeding

buff-bleed = Hemorragia
    .desc = Recibes daño periódico.

## Curse

buff-cursed = Maldito
    .desc = Sufres una maldición.

## Burning

buff-burn = En llamas
    .desc = Te estás quemando vivo.

## Crippled

buff-crippled = Incapacitado
    .desc = Te mueves con dificultad a causa de las heridas en tus piernas.

## Freeze

buff-frozen = Congelado
    .desc = Te mueves y atacas con más lentitud.

## Wet

buff-wet = Mojado
    .desc = El suelo te resulta resbaladizo por lo que te mueves con dificultad.

## Ensnared

buff-ensnared = Atrapado
    .desc = Tus piernas permanecen inmóviles debido a las lianas que las agarran.

## Fortitude

buff-fortitude = Aplomo
    .desc = Ningún ataque enemigo consigue aturdirte.

## Parried

buff-parried = Parada
    .desc = Tu arma ha sido parada por lo que te cuesta recuperarte.

## Potion sickness

buff-potionsickness = Enfermedad de poción
    .desc = La pociones te van haciendo cada vez menos efecto por tu consumo reciente de una.
    .stat =
        { $duration ->
            [1]
                Disminuye los efectos positivos de
                las siguientes pociones en { $strength } %.
                Durante { $duration } segundo.
           *[other]
                Disminuye los efectos positivos de
                las siguientes pociones en { $strength } %.
                Durante{ $duration } segundos.
        }

## Reckless

buff-reckless = Temerario
    .desc = Tus ataques se vuelven más fuertes, pero tus defensas disminuyen.

## Polymorped

buff-polymorphed = Polimorfismo
    .desc = La forma de tu cuerpo ha cambiado.

## Util

buff-remove = Haz click para eliminar
# Imminent Critical
buff-imminentcritical = Crítico Inminente
    .desc = Tu próximo ataque golpeará críticamente al enemigo.
# Berserk
buff-berserk = Berseker
    .desc = Estás en modo furioso, lo que hace que tus ataques sean más potentes y rápidos, y aumenta tu velocidad. Sin embargo, tu capacidad defensiva es menor.
# Bloodfeast
buff-bloodfeast = Festín de sangre
    .desc = Restauras vida en ataques contra enemigos que sangran.
buff-mysterious = Efecto misterioso
# Fury
buff-fury = Furia
    .desc = Con tu furia, tus golpes generarán más combo.
# Lifesteal
buff-lifesteal = Robo de vida
    .desc = Drena la fuerza vital de tus enemigos.
# Defiance
buff-defiance = Resistencia
    .desc = Puedes resistir golpes más fuertes y asombrosos y generar combos al ser golpeado, sin embargo es más lento.
# Sunderer
buff-sunderer = Destrozador
    .desc = Tus ataques pueden atravesar las defensas de tus enemigos y refrescarte con más energía.
# Salamander's Aspect
buff-salamanderaspect = Aspecto de salamandra
    .desc = No te quemas y te desplazas rápidamente por la lava.
# Frigid
buff-frigid = Gélido
    .desc = Congela a tus enemigos.
buff-agility = Agilidad
    .desc =
        Tus movimientos son más rápidos,
        pero generas menos daño y recibes más.
    .stat =
        { $duration ->
            [1]
                Aumenta la velocidad de movimiento en un { $strength } %.
                A cambio, tu ataque y defensa disminuyen drásticamente.
                durante { $duration } segundos.
           *[other]
                Aumenta la velocidad de movimiento en un { $strength } %.
                A cambio, tu ataque y defensa disminuyen drásticamente.
                durante { $duration } segundos.
        }
buff-resting_heal = Curación en Reposo
    .desc = Descansar cura { $rate } % de PS por segundo.
buff-combo_generation = Generador de combo
    .desc = Genera combo con el tiempo.
    .stat =
        { $duration ->
            [1] Genera { $str_total } de combo en { $duration } segundo.
           *[other] Genera { $str_total } de combo en { $duration } segundos.
        }
buff-poisoned = Envenenado
    .desc = Te sientes cada vez más débil...
buff-heatstroke = Golpe de calor
    .desc = Las altas temperaturas te han provocado un golpe de calor. Tu ganancia de energía y velocidad de movimiento se ven reducidas. Refréscate.
buff-scornfultaunt = Burla Descarada
    .desc = Te ríes descaradamente de tus enemigos, concediéndote fortaleza y energía reforzada. Sin embargo, tu muerte dará alas a tu asesino.
buff-rooted = Enraizado
    .desc = Estás atascado y no puedes moverte.
buff-winded = Sin Aliento
    .desc = Apenas puedes respirar lo que dificulta cuanta energía puedes recuperar y cuan rápido puedes moverte.
buff-concussion = Conmoción
    .desc = Has recibido un golpe fuerte en la cabeza y te cuesta concentrarte, lo que te impide usar algunos de tus ataques más complejos.
buff-staggered = Aturdido
    .desc = Has perdido el equilibrio y eres más vulnerable a los ataques fuertes.
buff-tenacity = Tenacidad
    .desc = No solo eres capaz de resistir ataques más pesados, sino que además te energizan. Sin embargo, eres más lento.
buff-resilience = Resiliencia
    .desc = Después de haber recibido un ataque debilitante, te vuelves más resistente a futuros efectos incapacitantes.
buff-owltalon = Garra de Búho
    .desc = Aprovechando que tu objetivo desconoce tu presencia, tu próximo ataque será más preciso e infligirá más daño.
buff-heavynock = Culata Pesada
    .desc = Coloca una flecha más pesada en tu arco, lo que permitirá que tu próximo disparo desestabilice al objetivo. Sin embargo, la flecha más pesada tendrá menos impulso a distancias más largas.
buff-heartseeker = Buscador de Corazones
    .desc = Tu próxima flecha golpeará a tu enemigo como si le hubiera infligido una herida al corazón, causándole una herida más grave y otorgándote energía.
buff-eagleeye = Ojo de Águila
    .desc = Podrás ver claramente los puntos vulnerables de tus objetivos y tendrás la agilidad necesaria para dirigir cada flecha hacia esas zonas.
buff-chilled = Enfriado
    .desc = El frío intenso te hace mover con más lentitud y te deja más vulnerable a ataques contundentes.
buff-ardenthunter = Cazador Apasionado
    .desc = Tu fervor hace que tus flechas sean más letales contra un objetivo concreto, y tu energía aumenta al ver cómo tus flechas lo alcanzan.
buff-ardenthunted = Cazado con Fervor
    .desc = Un arquero ferviente te ha marcado como objetivo.
buff-septicshot = Disparo séptico
    .desc = Tu próximo disparo provocará una infección en el objetivo, lo que aumentará su letalidad si este tiene alguna otra condición.
buff-shielded = Protegido
    .desc = Un escudo absorbe el daño entrante antes que tu salud.
    .stat = { $duration ->
        [1] Absorbe { $strength } de daño.
            Dura por { $duration } segundo.
        *[other] Absorbe { $strength } de daño.
                 Dura por { $duration } segundos.
    }
buff-bleeding_mark = Marca de Sangre
    .desc = Sangra con el tiempo, luego detona en una ráfaga cuando se agota - límpiala para detener la explosión.
buff-difficult_terrain = Terreno Difícil
    .desc = El suelo pelea cada paso - te mueves a velocidad reducida.
buff-freedom_of_movement = Libertad de Movimiento
    .desc = Te mueves sin obstáculos, ignorando terreno difícil.
buff-antimagic = Antimagia
    .desc = La magia falla aquí - no puedes lanzar hechizos y tus objetos mágicos están dormidos.
buff-anchored = Anclado
    .desc = Un ancla dimensional te mantiene en su lugar - no puedes teletransportarte o parpadear.
buff-asleep = Dormido
    .desc = Estás hundido en un sueño mágico - incapaz de moverte o actuar.
buff-blinded = Cegado
    .desc = No puedes ver para apuntar - tus ataques infligen daño reducido.
buff-slowed = Ralentizado
    .desc = Tu velocidad de movimiento está reducida.
buff-stormchaser = Perseguidor de Tormentas
    .desc = Te regodeas en la violencia, cuando tus flechas dan en el blanco tus ataques y movimientos se vuelven más rápidos.
buff-terrified = Aterrorizado
    .desc = El terror inunda tus miembros, ralentizando cada paso.
buff-charmed = Encantado
    .desc = No puedes traicionarte a ti mismo hiriendo a quienquiera que hizo esto por ti.
buff-dominated = Dominado
    .desc = Tu voluntad ya no te pertenece; obedecés las órdenes de quien te domina.
buff-maddened = Enloquecido
    .desc = Te volviste hostil contra todos, hasta contra tus antiguos aliados.
buff-paralyzed = Paralizado
    .desc = No podés moverte, actuar, ni defenderte.
buff-hollowtouched = Tocado por el Vacío
    .desc = El Vacío te ha probado. La salud máxima reducida.
buff-ardenthunt = Caza Apasionada
    .desc = Tu fervor hace que tus flechas sean más letales contra un objetivo específico, al costo de ser más débiles contra otros.
buff-ignitearrow = Flecha Incendiaria
    .desc = Has encendido tu próxima flecha, lo que hará que queme al objetivo que golpee, y puede ser explosiva si se dispara de cierta manera.
buff-freezearrow = Flecha Congelante
    .desc = Has congelado tu próxima flecha, lo que hará que congele al objetivo que golpee, y puede explotar en una nube de escarcha si se dispara de cierta manera.
buff-drencharrow = Flecha Envenenada
    .desc = Has empapado tu próxima flecha en veneno, lo que hará que envenene al objetivo que golpee, y puede explotar en una nube de veneno si se dispara de cierta manera.
buff-joltarrow = Flecha Eléctrica
    .desc = Has cargado estáticamente tu próxima flecha, lo que creará arcos de electricidad desde el objetivo que golpee, y puede ser atraída a objetivos si se dispara de cierta manera.
