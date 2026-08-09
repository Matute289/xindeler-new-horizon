## Xindeler: chrome for the Diary "Spells" tab.
##
## Spell *names* and *descriptions* are NOT here — they live next to each spell
## family in `spell/*.ftl` under the keys the compendium already points at.
## What this file holds is the tab's own text plus the shared enum names the
## per-spell metadata line is assembled from.

hud-diary-spells-empty = Your class knows no spells.
hud-diary-spells-locked = Requires { $class } level { $level }
hud-diary-spells-cantrip = Cantrip
hud-diary-spells-level = Level { $level }

## Schools of magic
hud-diary-spells-school-abjuration = Abjuration
hud-diary-spells-school-conjuration = Conjuration
hud-diary-spells-school-divination = Divination
hud-diary-spells-school-enchantment = Enchantment
hud-diary-spells-school-evocation = Evocation
hud-diary-spells-school-illusion = Illusion
hud-diary-spells-school-necromancy = Necromancy
hud-diary-spells-school-transmutation = Transmutation
hud-diary-spells-school-axiomancy = Axiomancy
hud-diary-spells-school-hemomancy = Hemomancy

## Where the power comes from
hud-diary-spells-source-arcane = Arcane
hud-diary-spells-source-divine = Divine
hud-diary-spells-source-primordial = Primordial
hud-diary-spells-source-psionic = Psionic
hud-diary-spells-source-ki = Ki

## Cast time
hud-diary-spells-cast-action = Action
hud-diary-spells-cast-bonus = Bonus action
hud-diary-spells-cast-reaction = Reaction
hud-diary-spells-cast-minutes = { $minutes } min cast

## Range
hud-diary-spells-range-self = Self
hud-diary-spells-range-touch = Touch
hud-diary-spells-range-meters = { $meters } m

## Duration
hud-diary-spells-duration-instant = Instant
hud-diary-spells-duration-secs = { $secs } s
hud-diary-spells-duration-concentration = Concentration { $secs } s

## Area of effect
hud-diary-spells-aoe-sphere = Sphere { $size } m
hud-diary-spells-aoe-cone = Cone { $size } m
hud-diary-spells-aoe-line = Line { $size } m
hud-diary-spells-aoe-cube = Cube { $size } m

## Per-source mastery header
hud-diary-spells-mastery-arcane-known = known by default
hud-diary-spells-mastery-pct = { $source } — { $pct }%
hud-diary-spells-mastery-empty-source = { $source } — no spells known yet
hud-diary-spells-mastery-tooltip-tier-2 = 25% mastery unlocks copying level 2 spells of this source
hud-diary-spells-mastery-tooltip-tier-4 = 50% mastery unlocks copying level 4 spells of this source
hud-diary-spells-mastery-tooltip-tier-6 = 75% mastery unlocks copying level 6 spells of this source
hud-diary-spells-mastery-tooltip-tier-all = 100% mastery unlocks copying every level of this source
hud-diary-spells-mastery-tooltip-no-content = No spells of this source exist yet
