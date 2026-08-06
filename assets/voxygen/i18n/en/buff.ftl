## Regeneration
buff-heal = Heal
    .desc = Gain health over time.
    .stat = { $duration ->
        [1] Restores { $str_total } health points over { $duration } second.
        *[other] Restores { $str_total } health points over { $duration } seconds.
    }
## Potion
buff-potion = Potion
    .desc = Drinking...
## Agility
buff-agility = Agility
    .desc =
        Your movement is faster,
        but you deal less damage and take more damage.
    .stat = { $duration ->
        [1] Increases movement speed by { $strength } %.
            In return, your attack and defense decrease drastically.
            Lasts for { $duration } second.
        *[other] Increases movement speed by { $strength } %.
                 In return, your attack and defense decrease drastically.
                 Lasts for { $duration } seconds.
    }
## Saturation
buff-saturation = Saturation
    .desc = Gain health over time from consumables.
## Campfire/Bed
buff-resting_heal = Resting Heal
    .desc = Resting heals { $rate } % HP per second.
## Energy Regen
buff-energy_regen = Energy Regeneration
    .desc = Faster energy regeneration.
    .stat = { $duration ->
        [1] Restores { $str_total } energy over { $duration } second.
        *[other] Restores { $str_total } energy over { $duration } seconds.
    }
## Combo Generation
buff-combo_generation = Combo Generation
    .desc = Generates combo over time.
    .stat = { $duration ->
        [1] Generates { $str_total } combo over { $duration } second.
        *[other] Generates { $str_total } combo over { $duration } seconds.
    }
## Health Increase
buff-increase_max_health = Increase Max Health
    .desc = Your maximum HP is increased.
    .stat = { $duration ->
        [1] Raises maximum health
            by { $strength }.
            Lasts for { $duration } second.
        *[other] Raises maximum health
                 by { $strength }.
                 Lasts for { $duration } seconds.
    }
## Shield (temp-HP absorb, BL-05)
buff-shielded = Shielded
    .desc = A shield absorbs incoming damage before your health.
    .stat = { $duration ->
        [1] Absorbs { $strength } damage.
            Lasts for { $duration } second.
        *[other] Absorbs { $strength } damage.
                 Lasts for { $duration } seconds.
    }
## Energy Increase
buff-increase_max_energy = Increase Max Energy
    .desc = Your maximum energy is increased.
    .stat = { $duration ->
        [1] Raises maximum energy
            by { $strength }.
            Lasts for { $duration } second.
        *[other] Raises maximum energy
                 by { $strength }.
                 Lasts for { $duration } seconds.
    }
## Invulnerability
buff-invulnerability = Invulnerability
    .desc = You cannot be damaged by any attack.
    .stat = { $duration ->
        [1] Grants invulnerability.
            Lasts for { $duration } second.
        *[other] Grants invulnerability.
                 Lasts for { $duration } seconds.
    }
## Protection Ward
buff-protectingward = Protecting Ward
    .desc = You are protected, somewhat, from attacks.
## Frenzied
buff-frenzied = Frenzied
    .desc = You are imbued with unnatural speed and can ignore minor injuries.
## Haste
buff-hastened = Hastened
    .desc = Your movements and attacks are faster.
## Bleeding
buff-bleed = Bleeding
    .desc = Inflicts regular damage.
## Bleeding Mark (bleed-detonate, BL-05)
buff-bleeding_mark = Bleeding Mark
    .desc = Bleeds over time, then detonates for a burst when it runs out - cleanse it to stop the blast.
## Curse
buff-cursed = Cursed
    .desc = You are cursed.
## Burning
buff-burn = On Fire
    .desc = You are burning alive.
## Crippled
buff-crippled = Crippled
    .desc = Your movement is crippled as your legs are heavily injured.
## Difficult Terrain (BL-03)
buff-difficult_terrain = Difficult Terrain
    .desc = The ground fights every step - you move at reduced speed.
buff-freedom_of_movement = Freedom of Movement
    .desc = You move unhindered, ignoring difficult terrain.
## Antimagic (BL-36)
buff-antimagic = Antimagic
    .desc = Magic fails here - you cannot cast and your magic items lie dormant.
## Spell riders (BL-05)
buff-anchored = Anchored
    .desc = A dimensional anchor holds you fast - you cannot teleport or blink.
buff-asleep = Asleep
    .desc = You are sunk in magical sleep - unable to move or act.
buff-blinded = Blinded
    .desc = You cannot see to aim - your attacks deal reduced damage.
## Slowed (BL-66 d)
buff-slowed = Slowed
    .desc = Your movement speed is reduced.
buff-agonized = Agonized
    .desc = Wracked with pain - your movement and attacks are weakened, and you cannot use auxiliary abilities.
## Freeze
buff-frozen = Frozen
    .desc = Your movements and attacks are slowed.
## Wet
buff-wet = Wet
    .desc = The ground rejects your feet, making it hard to stop.
## Poisoned
buff-poisoned = Poisoned
    .desc = You feel your life withering away...
## Ensnared
buff-ensnared = Ensnared
    .desc = Vines grasp at your legs, impeding your movement.
## Fortitude
buff-fortitude = Fortitude
    .desc = You can withstand staggers, and as you take more damage you stagger others more easily.
## Parried
buff-parried = Parried
    .desc = You were parried and now are slow to recover.
## Potion sickness
buff-potionsickness = Potion sickness
    .desc = Potions have less positive effect on you after recently consuming a potion.
    .stat = { $duration ->
        [1] Decreases the positive effects of
            subsequent potions by { $strength } %.
            Lasts for { $duration } second.
        *[other] Decreases the positive effects of
                 subsequent potions by { $strength } %.
                 Lasts for { $duration } seconds.
    }
## Reckless
buff-reckless = Reckless
    .desc = Your attacks are more powerful. However, you are leaving your defenses open.
## Polymorped
buff-polymorphed = Polymorphed
    .desc = Your body changes form.
## Frigid
buff-frigid = Frigid
    .desc = Freeze your foes.
## Lifesteal
buff-lifesteal = Lifesteal
    .desc = Siphon your enemies' life away.
## Salamander's Aspect
buff-salamanderaspect = Salamander's Aspect
    .desc = You cannot burn and you move fast through lava.
## Imminent Critical
buff-imminentcritical = Imminent Critical
    .desc = Your next attack will critically hit the enemy.
## Fury
buff-fury = Fury
    .desc = With your fury, your strikes generate more combo.
## Sunderer
buff-sunderer = Sunderer
    .desc = Your attacks can break through your foes' defences and refresh you with more energy.
## Defiance
buff-defiance = Defiance
    .desc = You can withstand mightier and more staggering blows and generate combo by being hit, however you are slower.
## Bloodfeast
buff-bloodfeast = Bloodfeast
    .desc = You restore life on attacks against bleeding enemies.
## Berserk
buff-berserk = Berserk
    .desc = You are in a berserking rage, causing your attacks to be more powerful and swift, and increasing your speed. However, as a result your defensive capability is less.
## Heatstroke
buff-heatstroke = Heatstroke
    .desc = You were exposed to heat and now suffer from heatstroke. Your energy reward and movement speed are cut down. Chill.
## Scornful Taunt
buff-scornfultaunt = Scornful Taunt
    .desc = You scornfully taunt your enemies, granting you bolstered fortitude and stamina. However, your death will bolster your killer.
## Rooted
buff-rooted = Rooted
    .desc = You are stuck in place and cannot move.
## Winded
buff-winded = Winded
    .desc = You can barely breathe hampering how much energy you can recover and how quickly you can move.
## Concussion
buff-concussion = Concussion
    .desc = You have been hit hard on the head and have trouble focusing, preventing you from using some of your more complex attacks.
## Staggered
buff-staggered = Staggered
    .desc = You are off balance and more susceptible to heavy attacks.
## Tenacity
buff-tenacity = Tenacity
    .desc = You are not only able to shrug off heavier attacks, they energize you as well. However you are also slower.
## Resilience
buff-resilience = Resilience
    .desc = After having just taken a debilitating attack, you become more resilient to future incapaciting effects.
## Storm Chaser
buff-stormchaser = Storm Chaser
    .desc = You revel in violence, when your arrows strike true your attacks and movements become faster.
## Owl Talon
buff-owltalon = Owl Talon
    .desc = Taking advantage of your target not knowing of your presence, your next attack will be more precise and deal more damage.
## Heavy Nock
buff-heavynock = Heavy Nock
    .desc = Nock a heavier arrow to your bow, allowing your next shot to stagger the target. The heavier arrow will have less momentum at longer ranges though.
## Heartseeker
buff-heartseeker = Heartseeker
    .desc = Your next arrow will strike your enemy as if it'd dealt a heartwound, causing a more serious wound and giving you energy.
## Eagle Eye
buff-eagleeye = Eagle Eye
    .desc = You can clearly see the vulnerable areas on your targets, and have the agility required to direct every arrow to those areas.
## Chilled
buff-chilled = Chilled
    .desc = The intense cold makes you a little slower and leaves you more vulnerable to forceful attacks.
## Ardent Hunter
buff-ardenthunter = Ardent Hunter
    .desc = Your fervor causes your arrows to be more lethal to a specific target, and your energy to rise as you see your arrows strike them.
## Aredent Hunted
buff-ardenthunted = Ardent Hunted
    .desc = You have been marked as a target by a fervent archer.
## Septic Shot
buff-septicshot = Septic Shot
    .desc = Your next arrow will cause infection in the target, making it more deadly if the target has any ongoing conditions.
## Terrified
buff-terrified = Terrified
    .desc = Dread floods your limbs, slowing every step.
## Charmed
buff-charmed = Charmed
    .desc = You cannot bring yourself to harm whoever did this to you.
buff-dominated = Dominated
    .desc = Your will is not your own; you obey your dominator's commands.
buff-maddened = Maddened
    .desc = You have turned against everyone, even your former allies.
buff-paralyzed = Paralyzed
    .desc = You cannot move, act, or fight back.
## Hollowtouched
buff-hollowtouched = Hollowtouched
    .desc = The Hollow has tasted you. Maximum health reduced.
## Ardent Hunt (upstream)
buff-ardenthunt = Ardent Hunt
    .desc = Your fervor causes your arrows to be more lethal to a specific target, at the cost of them being weaker against other targets.
## Ignite Arrow
buff-ignitearrow = Ignite Arrow
    .desc = You've ignited your next arrow, which will cause it to burn the target it strikes, and may be explosive if fired in a certain way.
## Freeze Arrow
buff-freezearrow = Freeze Arrow
    .desc = You've frozen your next arrow, which will cause it to freeze the target it strikes, and it may explode in a cloud of frost if fired in a certain way.
## Drench Arrow
buff-drencharrow = Drench Arrow
    .desc = You've drenched your next arrow in poison, which will cause it to poison the target it strikes, and it may explode in a cloud of poison if fired in a certain way.
## Jolt Arrow
buff-joltarrow = Jolt Arrow
    .desc = You've statically charged your next arrow, which will create arcs of electricity from the target it strikes, and it may be attracted to targets if fired in a certain way.
## Detecting (placeholder strings; content spells land separately)
buff-detecting = Detecting
    .desc = You are attuned to a magical sense.
## See Invisible (placeholder strings; content spells land separately)
buff-seeinvisible = See Invisible
    .desc = You can perceive concealed and invisible creatures.
## True Sight (placeholder strings; content spells land separately)
buff-truesight = True Sight
    .desc = You see through illusion, invisibility, and darkness.
## Remote Sensing (placeholder strings; content spells land separately)
buff-remotesensing = Remote Sensing
    .desc = Your senses are anchored elsewhere; your own body stands unattended.
## Disguised (placeholder strings; content spells land separately)
buff-disguised = Disguised
    .desc = You appear as something else. Your body, movement and reach are unchanged.
## Pass without Trace
buff-passwithouttrace = Pass without Trace
    .desc = A hush of shadow cloaks your footsteps and scent, making you far harder to notice.
## Mooncloak
buff-mooncloak = Mooncloak
    .desc = Shifting silver dimness veils your movements and dulls harmful magic against you.
## Nondetection
buff-nondetection = Nondetection
    .desc = A screening ward makes you immune to scrying and divination magic.
## Magic Aura
buff-magicaura = Magic Aura
    .desc = A subtle veil feeds false readings to anyone probing you with detection magic.
## Sequester
buff-sequester = Sequester
    .desc = A veil of stillness renders you unseen and shielded from every form of remote sensing.
## Smites (self-buff, next weapon hit gains a rider)
buff-blindingsmite = Blinding Smite
    .desc = Your next weapon strike sears with radiant light, dealing extra damage and blinding your foe.
buff-brandingsmite = Branding Smite
    .desc = Your next weapon strike sears with radiant light, dealing extra damage.
buff-divinesmite = Divine Smite
    .desc = Your next weapon strike is charged with holy power, dealing significant extra damage.
buff-searingsmite = Searing Smite
    .desc = Your next weapon strike sears with fire, dealing extra damage and setting your foe alight.
buff-staggeringsmite = Staggering Smite
    .desc = Your next weapon strike rattles the senses, dealing extra damage and knocking your foe off balance.
buff-thunderoussmite = Thunderous Smite
    .desc = Your next weapon strike booms like thunder, dealing extra damage and hurling your foe backward.
buff-wrathfulsmite = Wrathful Smite
    .desc = Your next weapon strike channels dread, dealing extra damage and filling your foe with terror.
## Bane / Bless / Faerie Fire / Enfeebled
buff-blessed = Blessed
    .desc = Your strikes and resolve are subtly sharpened.
buff-crusadersmantle = Crusader's Mantle
    .desc = A radius of consecrated light bolsters your strikes, wreathing every blow in searing radiance.
buff-restfulsleep = Restful Sleep
    .desc = You are sunk in a warded, restful sleep - unable to move or act, but any harm will wake you at once.
buff-otherworldlyward = Otherworldly Ward
    .desc = A radiant warding light surrounds you, dulling the blows of attackers while it holds.
buff-bane = Bane
    .desc = Your focus frays; your strikes and resolve falter at the worst moments.
buff-faeriefire = Faerie Fire
    .desc = A glow clings to you, leaving you unable to hide. Attacks against you land more easily.
buff-enfeebled = Enfeebled
    .desc = Your blows land weak, dealing only a fraction of their usual force.
## Util
buff-mysterious = Mysterious effect
buff-remove = Click to remove
