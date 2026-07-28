//! The three-layer creature taxonomy applied to the whole bestiary:
//! **kind → subtype → tags**.
//!
//! * [`CreatureKind`] — layers 1-2, the flat 14-value discriminant (Aberration,
//!   Beast, Celestial, Construct, Dragon, Elemental, Fey, Fiend, Giant,
//!   Humanoid, Monstrosity, Ooze, Plant, Undead). It stays **flat and
//!   `#[repr(u8)]`** on purpose: the damage-bonus channel that consumes this
//!   taxonomy is a `[f32; CreatureKind::NUM_KINDS]` indexed by the
//!   discriminant, which stops having an index the moment a payload is hung off
//!   the variants.
//! * [`CreatureSubtype`] — layer 3, a wrapper over one subtype enum *per* kind
//!   (`Undead(UndeadSubtype)`, `Humanoid(HumanoidSubtype)`, …). The wrapper
//!   always knows its own kind via [`CreatureSubtype::kind`], so an
//!   `Undead(Vampire)` can never report `Beast`.
//! * [`CreatureTags`] — layer 4, 34 universal, cross-cutting flavour tags as a
//!   `bitflags` over `u64`: 8 bytes, `Copy`, zero allocations, and "does it
//!   have tag X?" is one machine instruction. Explicitly **not** a
//!   `HashSet`/`Vec` — `Stats` is rebuilt by the buff system every tick.
//!
//! This is a classification axis, independent from [`Body`]'s own kind/shape
//! taxonomy. `Body::Humanoid` is a body *shape* (a bipedal, playable-race
//! skeleton); `CreatureKind::Humanoid` is a creature *kind* that many
//! non-`Body::Humanoid` bodies also belong to — a `BipedSmall(Gnome)` is a
//! Humanoid creature without being a `Body::Humanoid`. The two axes must
//! never be conflated; see [`Body::is_humanoid`], which intentionally keys
//! off body shape for rendering/persistence and is untouched by this module.
//!
//! `None` means "not a creature at all" (arrows, ships, most objects) and is
//! never a silent default for an unclassified species —
//! `every_creature_body_has_a_creature_kind` and its subtype sibling below
//! enforce that every creature-bearing `Body` kind returns `Some` from both.
//!
//! # Naming
//!
//! Subtype and tag names are Xindeler's own vocabulary, not another game's.
//! Terms that are public-domain folklore or plain English are used directly;
//! terms that would have been someone else's coinage carry the project's
//! replacement instead, and the reason is noted on the variant. A handful of
//! replacements are still provisional and say so inline.
//!
//! ⚪ `Ooze` and `Celestial` are near-empty on purpose: no shipped species is
//! an Ooze, and only `BirdLarge::Phoenix` is a Celestial. Those subtype enums
//! are forward-looking infrastructure, not dead code to be pruned.
use common_base::enum_iter;
use serde::{Deserialize, Serialize};
use strum::Display;

use crate::comp::body::{
    Body, arthropod, biped_large, biped_small, bird_large, bird_medium, crustacean, golem,
    humanoid, object, quadruped_low, quadruped_medium, quadruped_small, theropod,
};

// ───────────────────────── Layers 1-2: the flat kind ────────────────────────

enum_iter! {
    ~const_array(ALL)
    #[derive(Copy, Clone, Debug, Display, PartialEq, Eq, Hash, Serialize, Deserialize)]
    #[repr(u8)]
    pub enum CreatureKind {
        Aberration = 0,
        Beast = 1,
        Celestial = 2,
        Construct = 3,
        Dragon = 4,
        Elemental = 5,
        Fey = 6,
        Fiend = 7,
        Giant = 8,
        Humanoid = 9,
        Monstrosity = 10,
        Ooze = 11,
        Plant = 12,
        Undead = 13,
    }
}

// ─────────────────────── Layer 3: one enum per kind ────────────────────────

enum_iter! {
    #[derive(Copy, Clone, Debug, Display, PartialEq, Eq, Hash, Serialize, Deserialize)]
    #[repr(u8)]
    pub enum AberrationSubtype {
        /// "Astral" is Theosophical/occult vocabulary predating modern fantasy.
        AstralAberration,
        /// The chaos-spawn aberration. Replaces a borrowed creature name; a
        /// "X-like aberration" entry from the source list folds in here too,
        /// since a comparison is not a subtype.
        Churnborn,
        /// The abyssal, mind-dominating deep dwellers. Replaces a borrowed
        /// creature name.
        Deepwatcher,
        DreamEntity,
        EldritchHorror,
        /// The floating many-eyed tyrants. Replaces a borrowed creature name.
        EyeTyrant,
        LivingNightmare,
        /// The psionic brain-eaters. Replaces a borrowed creature name; the
        /// engine species id `BipedLarge::Mindflayer` is internal and
        /// deliberately not renamed here.
        MindEater,
        PsionicEntity,
        /// Kept in its Lovecraftian sense only (the star-spawn of Cthulhu,
        /// public domain); the later derived variants are never imported.
        StarSpawn,
        TentacledHorror,
        /// The nameless outer horrors. Canon forbids naming them individually,
        /// which is exactly why this is a subtype and not a species.
        Unspeakable,
        /// Entities from the Void — the canon term for the outer realm beyond
        /// the planes.
        VoidEntity,
    }
}

enum_iter! {
    #[derive(Copy, Clone, Debug, Display, PartialEq, Eq, Hash, Serialize, Deserialize)]
    #[repr(u8)]
    pub enum BeastSubtype {
        Avian,
        Bear,
        Canine,
        Caprine,
        /// ⚠️ Not the same thing as the body kind `Body::Crustacean`. This is
        /// the *creature subtype* of a Beast; `Body::Crustacean` is a body
        /// *shape*. They do not collide in Rust (`BeastSubtype::Crustacean`
        /// and `Body::Crustacean` are distinct paths), only conceptually.
        Crustacean,
        Dinosaur,
        Equine,
        Feline,
        Fish,
        Insect,
        Lizard,
        /// The generic warm-blooded bucket, used wherever no more specific
        /// subtype exists (lagomorphs, marsupials, bovids, bats…).
        Mammal,
        Primate,
        Reptile,
        Rodent,
        Serpent,
        Spider,
        Turtle,
        /// Kept alongside `Canine` because the engine ships real wolf species
        /// (`Wolf`, `Frostfang`) distinct from the generic canids.
        Wolf,
    }
}

enum_iter! {
    #[derive(Copy, Clone, Debug, Display, PartialEq, Eq, Hash, Serialize, Deserialize)]
    #[repr(u8)]
    pub enum CelestialSubtype {
        Angel,
        Archon,
        /// The feathered serpent, spelled from its Nahuatl root rather than
        /// from a borrowed transliteration.
        Coatl,
        Deva,
        Empyrean,
        Exarch,
        /// The East Asian 麒麟, spelled from its real transliteration rather
        /// than from a borrowed hyphenated form.
        Kirin,
        Messenger,
        Pegasus,
        Phoenix,
        SacredBeast,
        /// Top of the celestial ladder, named from the Pseudo-Dionysian
        /// choirs (public domain) rather than from a borrowed rank name.
        Seraph,
        /// The beast-shaped celestials. Replaces a borrowed creature name.
        Therion,
        /// Second rank of the Pseudo-Dionysian choirs; replaces a borrowed,
        /// wholly invented rank name with no folkloric root.
        Throne,
        /// The celestial-side unicorn; the fey-side one is
        /// [`FeySubtype::UnicornFey`]. The term legitimately belongs to two
        /// kinds, so it becomes two entries rather than one.
        Unicorn,
    }
}

enum_iter! {
    #[derive(Copy, Clone, Debug, Display, PartialEq, Eq, Hash, Serialize, Deserialize)]
    #[repr(u8)]
    pub enum ConstructSubtype {
        AnimatedArmor,
        AnimatedThing,
        ArcaneAutomaton,
        /// Gear-driven automata. This is a **subtype**, not a universal tag:
        /// it names a specific Construct identity rather than a property that
        /// any creature of any kind could carry.
        Clockwork,
        Colossus,
        Golem,
        Homunculus,
        /// The living-metal people. Replaces a borrowed setting-specific name,
        /// and lives on the Construct axis only even though the source list
        /// also proposed it as a Humanoid.
        Ironborn,
        LivingStatue,
        MagicalMachine,
        RuneConstruct,
        SoulConstruct,
        /// The bound protector construct; a generic name in place of a
        /// borrowed monster name built from the same two plain words.
        WardGolem,
    }
}

enum_iter! {
    #[derive(Copy, Clone, Debug, Display, PartialEq, Eq, Hash, Serialize, Deserialize)]
    #[repr(u8)]
    pub enum DragonSubtype {
        Chromatic,
        DeepDragon,
        Dragonkin,
        Dragonspawn,
        DragonTurtle,
        Drake,
        FaerieDragon,
        Gem,
        /// The eldest tier of true dragon, written as the generic compound
        /// rather than as a borrowed single-word coinage.
        GreatWyrm,
        Linnorm,
        Metallic,
        MoonDragon,
        MoonstoneDragon,
        PseudoDragon,
        SeaDragon,
        ShadowDragon,
        SunDragon,
        Wyvern,
    }
}

enum_iter! {
    #[derive(Copy, Clone, Debug, Display, PartialEq, Eq, Hash, Serialize, Deserialize)]
    #[repr(u8)]
    pub enum ElementalSubtype {
        Air,
        Ash,
        Dust,
        Earth,
        Fire,
        Ice,
        /// The source list contained this concept twice under two spellings;
        /// only this one survives.
        Lightning,
        LivingFlame,
        LivingStorm,
        Magma,
        Mud,
        /// Elementals of the plane of darkness — canon's own name for what
        /// another cosmology calls negative energy.
        Necrotic,
        /// Elementals of the plane of light — canon's own name for what
        /// another cosmology calls positive energy.
        Radiant,
        Salt,
        Smoke,
        Steam,
        Storm,
        Water,
    }
}

enum_iter! {
    #[derive(Copy, Clone, Debug, Display, PartialEq, Eq, Hash, Serialize, Deserialize)]
    #[repr(u8)]
    pub enum FeySubtype {
        Archfey,
        /// Already a de-facto rename of the borrowed teleporting hound.
        BlinkBeast,
        Dryad,
        /// The high fey of the bright courts. Replaces a borrowed coinage.
        FaeKin,
        Faerie,
        /// The blur-fast lesser fey. Replaces a borrowed coinage.
        Flickerkin,
        /// The generic English-folklore hag. ⚠️ The night-hag archetype is a
        /// fiend, not a fey, and lives at [`FiendSubtype::Nethercrone`].
        Hag,
        NatureSpirit,
        Nymph,
        Pixie,
        Redcap,
        /// Greek myth files satyrs with the nature spirits rather than with
        /// the peoples, so the term lives on the Fey axis only even though the
        /// source list also proposed it as a Humanoid.
        Satyr,
        Sprite,
        /// The animating spirit of a walking tree; the tree itself is
        /// [`PlantSubtype::Treant`].
        TreantSpirit,
        /// The fey-side unicorn; see [`CelestialSubtype::Unicorn`].
        UnicornFey,
    }
}

enum_iter! {
    #[derive(Copy, Clone, Debug, Display, PartialEq, Eq, Hash, Serialize, Deserialize)]
    #[repr(u8)]
    pub enum FiendSubtype {
        AbyssalHorror,
        Demon,
        DemonLord,
        Devil,
        /// Canon's third fiend race, neutral-evil mercenaries of the lower
        /// planes. Replaces a borrowed creature name; the spelling was chosen
        /// to avoid colliding with `biped_small::Species::Ashen`.
        Elderloth,
        FiendishDragon,
        HellKnight,
        Hellspawn,
        InfernalBeast,
        /// The dream-riding hag-fiends. Replaces a borrowed creature name.
        Nethercrone,
        /// The archdukes of the Nine Circles — canon's own term, used instead
        /// of adding a third synonym for the same rank. (A redundant second
        /// spelling of "demon" from the source list was dropped.)
        PitLord,
        Rakshasa,
    }
}

enum_iter! {
    #[derive(Copy, Clone, Debug, Display, PartialEq, Eq, Hash, Serialize, Deserialize)]
    #[repr(u8)]
    pub enum GiantSubtype {
        CloudGiant,
        Cyclops,
        /// Old English *eoten* / Norse *jötunn* — the public-domain root that
        /// covers trolls and thurses as well as two-headed giants. This is the
        /// bucket the engine's trolls and `Tursus` (literally *þurs*) use,
        /// since there is no separate `Troll` subtype.
        Ettin,
        FireGiant,
        FrostGiant,
        /// The grave-cold giants; a plain compound in place of a borrowed one.
        GraveGiant,
        HalfGiant,
        HillGiant,
        Ogre,
        StoneGiant,
        StormGiant,
        /// The primordial titans. This is a **subtype**, not a universal tag:
        /// it is a specific Giant identity, not a property any creature could
        /// carry.
        Titan,
    }
}

enum_iter! {
    #[derive(Copy, Clone, Debug, Display, PartialEq, Eq, Hash, Serialize, Deserialize)]
    #[repr(u8)]
    pub enum HumanoidSubtype {
        Aarakocra,
        Aasimar,
        /// The shapeless ooze-folk. Replaces a borrowed setting-specific name.
        Amorph,
        /// The astral-raiding half of a sundered people. Canon name, replacing
        /// a borrowed one.
        AstralReaver,
        /// The beast-kin who wear a partial animal aspect. Replaces a borrowed
        /// setting-specific name, and doubles as the bucket for the engine's
        /// dog-folk and hyena-folk.
        Beastblooded,
        Bugbear,
        Centaur,
        /// The British/Celtic folkloric changeling only; the borrowed
        /// setting's changeling *culture* is not imported.
        Changeling,
        /// The small clockwork-built folk. Replaces a borrowed
        /// setting-specific name, which also could not have kept "Gnome" here.
        Cogling,
        Dragonborn,
        Dwarf,
        Elf,
        Firbolg,
        Genasi,
        /// ⚠️ In Xindeler `Gnome` is the blue small-folk lineage, not the
        /// tinker gnome of other settings.
        Gnome,
        Goblin,
        Goliath,
        HalfElf,
        HalfOrc,
        Halfling,
        /// The hare-folk. Replaces a borrowed coinage.
        Harefolk,
        /// The hippo-folk. Replaces a borrowed setting-specific name.
        Hippokin,
        Hobgoblin,
        Human,
        Kenku,
        Kobold,
        Lizardfolk,
        /// The meditative, mind-disciplined half of a sundered people. Canon
        /// name, replacing a borrowed one.
        Mindbound,
        Minotaur,
        Orc,
        /// The serpent-folk lineage id; "serpent-folk" stays the generic
        /// descriptor of the people. Replaces a borrowed creature name.
        Serpenti,
        /// The one people from which the astral and mind-bound factions broke
        /// apart. Replaces a borrowed creature name.
        // nombre propuesto, sin confirmación del dueño del canon todavía
        Sundered,
        Tabaxi,
        Tiefling,
        Tortle,
        Triton,
        /// The two-souled, spirit-bonded folk. Replaces a borrowed
        /// setting-specific name.
        Twinsouled,
        /// The umbral elves, tied to the canon plane Umbriel — an Elf heritage
        /// rather than a separate people. Replaces a borrowed name for the
        /// same archetype.
        Umbrel,
        // Deliberately absent: `Satyr` and the living-metal folk, which live on
        // the Fey and Construct axes instead; three niche lineages the lineage
        // catalogue already excludes; and four crossover lineages ruled out of
        // scope for the project entirely.
    }
}

enum_iter! {
    #[derive(Copy, Clone, Debug, Display, PartialEq, Eq, Hash, Serialize, Deserialize)]
    #[repr(u8)]
    pub enum MonstrositySubtype {
        Basilisk,
        Chimera,
        Cockatrice,
        /// The burrowing ambush-predator archetype — a land shark that erupts
        /// from below. Replaces a borrowed creature name.
        Delvemaw,
        /// The burrowing fire-worm, hot enough to melt the rock it swims
        /// through. Replaces a borrowed creature name, and is distinct from
        /// the canon `Rimewyrm` (its frost counterpart).
        // nombre propuesto, sin confirmación del dueño del canon todavía
        Emberworm,
        Griffon,
        Hippogriff,
        Hydra,
        /// The colossal deep-burrowing worm. Canon name.
        Korrovax,
        KrakenSpawn,
        Manticore,
        Medusa,
        Mimic,
        Roc,
        RustMonster,
        SeaSerpent,
        /// The displacing shadow-cat whose image is never where its body is.
        /// Canon name, replacing a borrowed one.
        Shadepard,
        Sphinx,
        /// The great talon-and-beak bear. A brand-neutral name for an
        /// archetype whose usual name is unmistakably another game's.
        // renombre por marca, sin confirmación del dueño del canon todavía
        Talonbear,
        /// The sealed colossus of canon. Note the engine species
        /// `QuadrupedMedium::Tarasque` keeps the public-domain Provençal
        /// single-r spelling of the folkloric beast and is correct as it is —
        /// the double-r form is never written.
        Terrorath,
        WinterWolf,
        Worg,
        Yeti,
    }
}

enum_iter! {
    #[derive(Copy, Clone, Debug, Display, PartialEq, Eq, Hash, Serialize, Deserialize)]
    #[repr(u8)]
    pub enum OozeSubtype {
        AcidOoze,
        /// The engulfing jelly; a colour swap keeps the creature and drops a
        /// borrowed name.
        AmberJelly,
        /// The black corrosive sludge. Replaces a borrowed creature name.
        BlackSump,
        /// The transparent corridor-filling cube. Replaces a borrowed creature
        /// name.
        Clearmaw,
        LivingSlime,
        PlasmaOoze,
        SentientOoze,
    }
}

enum_iter! {
    #[derive(Copy, Clone, Debug, Display, PartialEq, Eq, Hash, Serialize, Deserialize)]
    #[repr(u8)]
    pub enum PlantSubtype {
        AwakenedTree,
        Blight,
        Fungus,
        LivingPlant,
        Mushroom,
        Shrieker,
        /// The sapient fungus-folk. Replaces a borrowed creature name and
        /// aligns with the engine's own `QuadrupedSmall::Fungome`.
        // nombre propuesto, sin confirmación del dueño del canon todavía
        Sporekin,
        /// The walking tree. Kept as-is because renaming it would mean
        /// renaming two shipped engine species (`Golem::Treant`,
        /// `QuadrupedSmall::TreantSapling`), which is out of scope here.
        // decisión de marca pendiente del dueño del canon
        Treant,
        VineHorror,
        WorldTreeSpirit,
    }
}

enum_iter! {
    #[derive(Copy, Clone, Debug, Display, PartialEq, Eq, Hash, Serialize, Deserialize)]
    #[repr(u8)]
    pub enum UndeadSubtype {
        Banshee,
        Boneclaw,
        /// The undead dragon. Replaces a borrowed setting-specific name and
        /// aligns with the canon faction the Wyrm-Crowned.
        // nombre propuesto, sin confirmación del dueño del canon todavía
        Bonewyrm,
        /// The burning flying skull. Replaces a borrowed setting-specific name.
        Cinderskull,
        DeathKnight,
        /// The risen eye-tyrant — taxonomically an aberration, but a creature
        /// can only carry one kind, and its undeath is what the game reacts
        /// to. See [`AberrationSubtype::EyeTyrant`] for the living form.
        /// No shipped species uses this yet.
        // nombre propuesto, sin confirmación del dueño del canon todavía
        DeathlessEye,
        /// The withered remnant of a lich, reduced to dust and a skull.
        /// Replaces a borrowed creature name.
        DustLich,
        /// Lovecraft's own word (the ghasts of the Vaults of Zin), public
        /// domain and older than its use elsewhere.
        Ghast,
        Ghost,
        Ghoul,
        Lich,
        Mummy,
        Nightwalker,
        /// The gibbering, madness-spreading spirit. Replaces a borrowed
        /// coinage with no folkloric root.
        Ravewraith,
        Revenant,
        Shadow,
        Skeleton,
        Specter,
        Vampire,
        Wight,
        WillOWisp,
        Wraith,
        Zombie,
    }
}

enum_iter! {
    #[derive(Copy, Clone, Debug, Display, PartialEq, Eq, Hash, Serialize, Deserialize)]
    pub enum CreatureSubtype {
        Aberration(AberrationSubtype),
        Beast(BeastSubtype),
        Celestial(CelestialSubtype),
        Construct(ConstructSubtype),
        Dragon(DragonSubtype),
        Elemental(ElementalSubtype),
        Fey(FeySubtype),
        Fiend(FiendSubtype),
        Giant(GiantSubtype),
        Humanoid(HumanoidSubtype),
        Monstrosity(MonstrositySubtype),
        Ooze(OozeSubtype),
        Plant(PlantSubtype),
        Undead(UndeadSubtype),
    }
}

impl CreatureSubtype {
    /// The invariant that makes the three-type split safe: a subtype always
    /// knows its own kind. An `Undead(Vampire)` cannot report `Beast`.
    ///
    /// `subtype_and_kind_never_contradict_each_other` pins the *other* half of
    /// the invariant — that [`Body::creature_subtype`] and
    /// [`Body::creature_kind`] agree for every shipped species.
    pub const fn kind(self) -> CreatureKind {
        match self {
            CreatureSubtype::Aberration(_) => CreatureKind::Aberration,
            CreatureSubtype::Beast(_) => CreatureKind::Beast,
            CreatureSubtype::Celestial(_) => CreatureKind::Celestial,
            CreatureSubtype::Construct(_) => CreatureKind::Construct,
            CreatureSubtype::Dragon(_) => CreatureKind::Dragon,
            CreatureSubtype::Elemental(_) => CreatureKind::Elemental,
            CreatureSubtype::Fey(_) => CreatureKind::Fey,
            CreatureSubtype::Fiend(_) => CreatureKind::Fiend,
            CreatureSubtype::Giant(_) => CreatureKind::Giant,
            CreatureSubtype::Humanoid(_) => CreatureKind::Humanoid,
            CreatureSubtype::Monstrosity(_) => CreatureKind::Monstrosity,
            CreatureSubtype::Ooze(_) => CreatureKind::Ooze,
            CreatureSubtype::Plant(_) => CreatureKind::Plant,
            CreatureSubtype::Undead(_) => CreatureKind::Undead,
        }
    }
}

// ───────────────────────── Layer 4: the universal tags ──────────────────────

bitflags::bitflags! {
    /// The 34 universal, cross-cutting creature tags — orthogonal to both
    /// [`CreatureKind`] and [`CreatureSubtype`], so any creature of any kind
    /// may carry any of them.
    ///
    /// ⛔ **Deliberately a `bitflags` over `u64`, never a `HashSet`/`Vec`.**
    /// `Stats` is a net-synced component the buff system rebuilds every tick,
    /// and the questions gameplay actually asks ("does it have `UNDYING`?",
    /// "does it have any of `INFERNAL | VAMPIRIC`?") are a single `AND` plus a
    /// comparison. 8 bytes, `Copy`, zero allocations, trivially `Serialize`;
    /// 30 bits are left free to grow.
    ///
    /// `Clockwork` and `Titan` are **not** here: they name specific Construct
    /// and Giant identities and live on the subtype axis. `AQUATIC` and
    /// `SWARM` are here rather than on the subtype axis for the mirror-image
    /// reason — they apply transversally to creatures of any kind.
    #[derive(Copy, Clone, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
    pub struct CreatureTags: u64 {
        const ANCIENT       = 1 << 0;
        const AQUATIC       = 1 << 1;
        const ASTRAL        = 1 << 2;
        const COLD          = 1 << 3;
        const CORRUPTED     = 1 << 4;
        const COSMIC        = 1 << 5;
        /// The lightless world beneath the surface, as a generic descriptor.
        /// The specific canon place (Nythraldeep) is a location, not a tag,
        /// and the two never merge.
        const DEEPLANDS     = 1 << 6;
        const DIVINE        = 1 << 7;
        const ELITE         = 1 << 8;
        const ETHEREAL      = 1 << 9;
        const EXTRAPLANAR   = 1 << 10;
        const FLYING        = 1 << 11;
        const HUGE          = 1 << 12;
        const INFERNAL      = 1 << 13;
        /// The adjective only. ⛔ Never ship a UI string that turns this into
        /// another game's mechanic labels ("Legendary Actions"/"Resistance").
        const LEGENDARY     = 1 << 14;
        /// Mixed heritage. Named this way rather than "half-breed", which
        /// reads badly in a player-facing tag; the lineage catalogue already
        /// uses this word for the same concept.
        const MIXBLOOD      = 1 << 15;
        const MOUNTED       = 1 << 16;
        /// The adjective only — same caveat as `LEGENDARY`.
        const MYTHIC        = 1 << 17;
        const NAMED         = 1 << 18;
        const PLANAR        = 1 << 19;
        const PRIMORDIAL    = 1 << 20;
        const PSIONIC       = 1 << 21;
        const ROYAL         = 1 << 22;
        const SACRED        = 1 << 23;
        const SEA           = 1 << 24;
        const SHADOW        = 1 << 25;
        const SHAPECHANGER  = 1 << 26;
        const SIEGE         = 1 << 27;
        const SPELLCASTER   = 1 << 28;
        const SWARM         = 1 << 29;
        const UNIQUE        = 1 << 30;
        const UNDYING       = 1 << 31;
        const VAMPIRIC      = 1 << 32;
        const VOID          = 1 << 33;
    }
}

impl Body {
    /// The flat 14-kind creature classification for this body, or `None` if
    /// this body is not a creature at all (an arrow, a ship, a training
    /// dummy, most other [`object::Body`]s, ...).
    ///
    /// `None` must never be a silent catch-all for a species that was simply
    /// forgotten. Every arm below is enumerated per body kind; a `_ =>`
    /// default is only ever used to assign the *dominant* kind within a
    /// body kind (e.g. `QuadrupedSmall`'s `_ => Beast`), never to fall
    /// through to `None` for a real creature.
    /// `every_creature_body_has_a_creature_kind` (below) is the
    /// compile-and-test guard for this.
    pub fn creature_kind(&self) -> Option<CreatureKind> {
        match self {
            // The 6 playable races. Draugr is a living lineage (like a
            // dhampir), not a raised corpse, so it classifies as Humanoid,
            // not Undead. This is independent of `Body::is_humanoid`, which
            // keys off body *shape*, not creature *kind*.
            Body::Humanoid(_) => Some(CreatureKind::Humanoid),

            Body::QuadrupedSmall(b) => Some(match b.species {
                quadruped_small::Species::Fungome | quadruped_small::Species::TreantSapling => {
                    CreatureKind::Plant
                },
                _ => CreatureKind::Beast,
            }),

            Body::QuadrupedMedium(b) => Some(match b.species {
                quadruped_medium::Species::Grolgar
                | quadruped_medium::Species::Tarasque
                | quadruped_medium::Species::Catoblepas
                | quadruped_medium::Species::Roshwalr
                | quadruped_medium::Species::Dreadhorn
                | quadruped_medium::Species::Akhlut
                | quadruped_medium::Species::Darkhound => CreatureKind::Monstrosity,
                quadruped_medium::Species::Kelpie | quadruped_medium::Species::Hirdrasil => {
                    CreatureKind::Fey
                },
                quadruped_medium::Species::Bonerattler => CreatureKind::Undead,
                quadruped_medium::Species::Barghest => CreatureKind::Fiend,
                quadruped_medium::Species::ClaySteed => CreatureKind::Construct,
                _ => CreatureKind::Beast,
            }),

            Body::QuadrupedLow(b) => Some(match b.species {
                quadruped_low::Species::Hakulaq
                | quadruped_low::Species::Basilisk
                | quadruped_low::Species::Rocksnapper
                | quadruped_low::Species::Rootsnapper
                | quadruped_low::Species::Reefsnapper
                | quadruped_low::Species::Elbst
                | quadruped_low::Species::Snaretongue
                | quadruped_low::Species::Hydra => CreatureKind::Monstrosity,
                quadruped_low::Species::Lavadrake
                | quadruped_low::Species::Icedrake
                | quadruped_low::Species::Mossdrake => CreatureKind::Dragon,
                quadruped_low::Species::Maneater | quadruped_low::Species::Deadwood => {
                    CreatureKind::Plant
                },
                quadruped_low::Species::Dagon => CreatureKind::Aberration,
                _ => CreatureKind::Beast,
            }),

            Body::BirdMedium(b) => Some(match b.species {
                bird_medium::Species::BloodmoonBat => CreatureKind::Monstrosity,
                // VampireBat stays Beast deliberately: it's Desmodus
                // rotundus, a real animal with real infrared receptors, not
                // an actual vampire — its use as a themed dungeon spawn is
                // flavour, not taxonomy.
                _ => CreatureKind::Beast,
            }),

            Body::BirdLarge(b) => Some(match b.species {
                bird_large::Species::Cockatrice | bird_large::Species::Roc => {
                    CreatureKind::Monstrosity
                },
                // The only Celestial in the bestiary — a solar rebirth-bird.
                bird_large::Species::Phoenix => CreatureKind::Celestial,
                _ => CreatureKind::Dragon,
            }),

            Body::FishSmall(_) => Some(CreatureKind::Beast),
            Body::FishMedium(_) => Some(CreatureKind::Beast),

            // No dominant kind here (Giant is the largest bucket at 10/35)
            // — every species is named explicitly, no `_ =>` default, so a
            // future species added to this enum is a compile error here
            // rather than a silent fall-through.
            Body::BipedLarge(b) => Some(match b.species {
                biped_large::Species::Ogre
                | biped_large::Species::Cyclops
                | biped_large::Species::Cavetroll
                | biped_large::Species::Mountaintroll
                | biped_large::Species::Swamptroll
                | biped_large::Species::Blueoni
                | biped_large::Species::Redoni
                | biped_large::Species::Gigasfrost
                | biped_large::Species::Gigasfire
                | biped_large::Species::Tursus => CreatureKind::Giant,
                biped_large::Species::Harvester
                | biped_large::Species::HaniwaGeneral
                | biped_large::Species::TerracottaBesieger
                | biped_large::Species::TerracottaDemolisher
                | biped_large::Species::TerracottaPunisher
                | biped_large::Species::TerracottaPursuer
                | biped_large::Species::Forgemaster => CreatureKind::Construct,
                biped_large::Species::Occultsaurok
                | biped_large::Species::Mightysaurok
                | biped_large::Species::Slysaurok
                | biped_large::Species::AdletElder
                | biped_large::Species::SeaBishop
                | biped_large::Species::Cultistwarlord
                | biped_large::Species::Cultistwarlock => CreatureKind::Humanoid,
                biped_large::Species::Dullahan
                | biped_large::Species::Huskbrute
                | biped_large::Species::Strigoi
                | biped_large::Species::Cursekeeper
                | biped_large::Species::Executioner => CreatureKind::Undead,
                biped_large::Species::Wendigo
                | biped_large::Species::Werewolf
                | biped_large::Species::Minotaur
                | biped_large::Species::Yeti
                | biped_large::Species::Tidalwarrior => CreatureKind::Monstrosity,
                biped_large::Species::Mindflayer => CreatureKind::Aberration,
            }),

            // Same reasoning as `BipedLarge`: no dominant kind, every
            // species named explicitly, no `_ =>` default.
            Body::BipedSmall(b) => Some(match b.species {
                biped_small::Species::Gnome
                | biped_small::Species::Sahagin
                | biped_small::Species::Adlet
                | biped_small::Species::Gnarling
                | biped_small::Species::GnarlingChieftain
                | biped_small::Species::GoblinThug
                | biped_small::Species::GoblinChucker
                | biped_small::Species::GoblinRuffian
                | biped_small::Species::Gnoll => CreatureKind::Humanoid,
                biped_small::Species::Mandragora
                | biped_small::Species::Cactid
                | biped_small::Species::Bushly
                | biped_small::Species::Irrwurz
                | biped_small::Species::GreenLegoom
                | biped_small::Species::OchreLegoom
                | biped_small::Species::PurpleLegoom
                | biped_small::Species::RedLegoom
                | biped_small::Species::UmberLegoom => CreatureKind::Plant,
                biped_small::Species::Husk
                | biped_small::Species::Jiangshi
                | biped_small::Species::ShamanicSpirit
                | biped_small::Species::BloodmoonHeiress
                | biped_small::Species::Bloodservant
                | biped_small::Species::Harlequin => CreatureKind::Undead,
                biped_small::Species::Haniwa
                | biped_small::Species::Myrmidon
                | biped_small::Species::IronDwarf
                | biped_small::Species::TreasureEgg => CreatureKind::Construct,
                // World-boss summons with their own dedicated body models
                // (not reskinned humanoids wearing themed armor), so they
                // classify as the element they're themed on rather than as
                // Humanoid or Construct.
                biped_small::Species::Boreal
                | biped_small::Species::Ashen
                | biped_small::Species::Flamekeeper => CreatureKind::Elemental,
                biped_small::Species::Kappa => CreatureKind::Fey,
            }),

            Body::Theropod(b) => Some(match b.species {
                theropod::Species::Yale => CreatureKind::Monstrosity,
                _ => CreatureKind::Beast,
            }),

            Body::Arthropod(b) => Some(match b.species {
                arthropod::Species::Moltencrawler
                | arthropod::Species::Mosscrawler
                | arthropod::Species::Sandcrawler => CreatureKind::Monstrosity,
                arthropod::Species::Dagonite => CreatureKind::Aberration,
                _ => CreatureKind::Beast,
            }),

            Body::Crustacean(b) => Some(match b.species {
                crustacean::Species::Crab | crustacean::Species::SoldierCrab => CreatureKind::Beast,
                crustacean::Species::Karkatha => CreatureKind::Monstrosity,
            }),

            Body::Dragon(_) => Some(CreatureKind::Dragon),

            // DO NOT collapse this arm into `Body::Golem(_) =>
            // Some(CreatureKind::Construct)`. 2 of 9 Golem species are not
            // Constructs: a Treant is a Plant (a living, ambulatory tree),
            // and a Mogwai is named after a demon and classifies as Fiend.
            // Pinned by `golem_treant_is_plant_and_mogwai_is_fiend`.
            Body::Golem(b) => Some(match b.species {
                golem::Species::StoneGolem
                | golem::Species::ClayGolem
                | golem::Species::WoodGolem
                | golem::Species::CoralGolem
                | golem::Species::IronGolem
                | golem::Species::AncientEffigy
                // In the same tomb-figurine line as Haniwa/HaniwaGeneral,
                // which are unambiguous constructs.
                | golem::Species::Gravewarden => CreatureKind::Construct,
                golem::Species::Treant => CreatureKind::Plant,
                golem::Species::Mogwai => CreatureKind::Fiend,
            }),

            // DO NOT collapse this arm into `Body::Object(_) =>
            // Some(CreatureKind::Construct)`. This is a deliberate allowlist
            // of the combat-capable, HP-bearing objects, not a blanket.
            // Everything else (arrows, firework shells, campfires,
            // `TrainingDummy`) is not a creature. In particular
            // `object::Body::TrainingDummy` MUST stay `None`: it is the one
            // object deliberately exempt from `Charmed` immunity (see
            // `Body::immune_to`). Pinned by `training_dummy_is_not_a_creature`
            // and `combat_capable_objects_are_constructs`.
            Body::Object(o) => match o {
                object::Body::TerracottaStatue
                | object::Body::HaniwaSentry
                | object::Body::ArrowTurret
                | object::Body::BarrelOrgan
                | object::Body::GnarlingTotemRed
                | object::Body::GnarlingTotemGreen
                | object::Body::GnarlingTotemWhite
                | object::Body::Crux => Some(CreatureKind::Construct),
                _ => None,
            },

            // Not creatures.
            Body::Item(_) | Body::Ship(_) | Body::Plugin(_) => None,
        }
    }

    /// The layer-3 subtype for this body, or `None` for the same non-creature
    /// bodies [`Body::creature_kind`] returns `None` for.
    ///
    /// 🔴 The two methods must agree for every species:
    /// `subtype_and_kind_never_contradict_each_other` asserts
    /// `creature_subtype().kind() == creature_kind()` across `Body::iter()`.
    /// That test is the *only* safety net against a mistyped subtype — the
    /// compiler cannot catch a species that is `Monstrosity` in one match and
    /// `Beast(...)` in the other.
    ///
    /// Species whose closest available subtype is an approximation (there is
    /// no `Amphibian`, `Troll`, `Lagomorph` or `Mollusc` subtype, and several
    /// engine-original creatures have no folkloric analogue at all) carry an
    /// inline `subtipo aproximado, sin match obvio` marker so a later pass can
    /// revisit them without re-deriving the whole table.
    pub fn creature_subtype(&self) -> Option<CreatureSubtype> {
        use CreatureSubtype as S;

        match self {
            Body::Humanoid(b) => Some(S::Humanoid(match b.species {
                // Xindeler's `Gnome` subtype IS the blue small-folk lineage.
                humanoid::Species::Danari => HumanoidSubtype::Gnome,
                humanoid::Species::Dwarf => HumanoidSubtype::Dwarf,
                humanoid::Species::Elf => HumanoidSubtype::Elf,
                humanoid::Species::Human => HumanoidSubtype::Human,
                humanoid::Species::Orc => HumanoidSubtype::Orc,
                // subtipo aproximado, sin match obvio — a grave-touched living
                // lineage. There are planar-touched human lineages
                // (Aasimar/Tiefling/Genasi) but no death-touched one, so it
                // falls back to its parent stock.
                humanoid::Species::Draugr => HumanoidSubtype::Human,
            })),

            Body::QuadrupedSmall(b) => Some(match b.species {
                quadruped_small::Species::Fungome => S::Plant(PlantSubtype::Sporekin),
                quadruped_small::Species::TreantSapling => S::Plant(PlantSubtype::Treant),

                quadruped_small::Species::Cat => S::Beast(BeastSubtype::Feline),
                quadruped_small::Species::Fox | quadruped_small::Species::Dog => {
                    S::Beast(BeastSubtype::Canine)
                },
                quadruped_small::Species::Sheep | quadruped_small::Species::Goat => {
                    S::Beast(BeastSubtype::Caprine)
                },
                quadruped_small::Species::Rat
                | quadruped_small::Species::Squirrel
                | quadruped_small::Species::Porcupine
                | quadruped_small::Species::Beaver => S::Beast(BeastSubtype::Rodent),
                quadruped_small::Species::Gecko => S::Beast(BeastSubtype::Lizard),
                quadruped_small::Species::Turtle => S::Beast(BeastSubtype::Turtle),
                // subtipo aproximado, sin match obvio — amphibians; there is
                // no `Amphibian` subtype, and `Reptile` is the nearest herp
                // bucket.
                quadruped_small::Species::Frog | quadruped_small::Species::Axolotl => {
                    S::Beast(BeastSubtype::Reptile)
                },
                // subtipo aproximado, sin match obvio — a gastropod; `Insect`
                // is the only invertebrate bucket that is not a spider or a
                // crustacean.
                quadruped_small::Species::MossySnail => S::Beast(BeastSubtype::Insect),
                // Everything else is an ordinary mammal: pigs and boars,
                // mustelids, lagomorphs (`Rabbit`/`Hare`/`Jackalope`),
                // marsupials (`Quokka`), pinnipeds (`Seal`), the hyena
                // (feliform, so neither cat nor dog), and the engine originals
                // `Batfox`, `Holladon` and `Truffler`.
                // subtipo aproximado, sin match obvio — for those last five in
                // particular: they really are mammals, just not any more
                // specific subtype that exists here.
                _ => S::Beast(BeastSubtype::Mammal),
            }),

            Body::QuadrupedMedium(b) => Some(match b.species {
                // subtipo aproximado, sin match obvio — an engine-original
                // hulking clawed predator with no folkloric analogue.
                quadruped_medium::Species::Grolgar => S::Monstrosity(MonstrositySubtype::Manticore),
                quadruped_medium::Species::Tarasque => {
                    S::Monstrosity(MonstrositySubtype::Terrorath)
                },
                // The catoblepas of the medieval bestiaries kills with its
                // gaze, which is the `Basilisk` archetype here.
                quadruped_medium::Species::Catoblepas => {
                    S::Monstrosity(MonstrositySubtype::Basilisk)
                },
                // subtipo aproximado, sin match obvio — an arctic sea-beast;
                // `KrakenSpawn` is the only marine-monster bucket.
                quadruped_medium::Species::Roshwalr => {
                    S::Monstrosity(MonstrositySubtype::KrakenSpawn)
                },
                // subtipo aproximado, sin match obvio — a composite horned
                // monster.
                quadruped_medium::Species::Dreadhorn => S::Monstrosity(MonstrositySubtype::Chimera),
                // subtipo aproximado, sin match obvio — the Inuit orca-wolf;
                // `WinterWolf` is the nearest arctic canid-monster.
                quadruped_medium::Species::Akhlut => S::Monstrosity(MonstrositySubtype::WinterWolf),
                quadruped_medium::Species::Darkhound => S::Monstrosity(MonstrositySubtype::Worg),

                quadruped_medium::Species::Kelpie | quadruped_medium::Species::Hirdrasil => {
                    S::Fey(FeySubtype::NatureSpirit)
                },
                quadruped_medium::Species::Bonerattler => S::Undead(UndeadSubtype::Skeleton),
                quadruped_medium::Species::Barghest => S::Fiend(FiendSubtype::InfernalBeast),
                quadruped_medium::Species::ClaySteed => S::Construct(ConstructSubtype::Golem),

                quadruped_medium::Species::Saber
                | quadruped_medium::Species::Tiger
                | quadruped_medium::Species::Lion
                | quadruped_medium::Species::Snowleopard => S::Beast(BeastSubtype::Feline),
                quadruped_medium::Species::Wolf | quadruped_medium::Species::Frostfang => {
                    S::Beast(BeastSubtype::Wolf)
                },
                quadruped_medium::Species::Tuskram | quadruped_medium::Species::Mouflon => {
                    S::Beast(BeastSubtype::Caprine)
                },
                quadruped_medium::Species::Donkey
                | quadruped_medium::Species::Zebra
                | quadruped_medium::Species::Horse => S::Beast(BeastSubtype::Equine),
                quadruped_medium::Species::Panda | quadruped_medium::Species::Bear => {
                    S::Beast(BeastSubtype::Bear)
                },
                // A ceratopsian-shaped engine original.
                quadruped_medium::Species::Ngoubou => S::Beast(BeastSubtype::Dinosaur),
                // Cervids, bovids, camelids, proboscideans and suids — all
                // correctly mammals, none with a more specific subtype here.
                _ => S::Beast(BeastSubtype::Mammal),
            }),

            Body::QuadrupedLow(b) => Some(match b.species {
                quadruped_low::Species::Hakulaq | quadruped_low::Species::Elbst => {
                    S::Monstrosity(MonstrositySubtype::SeaSerpent)
                },
                quadruped_low::Species::Basilisk => S::Monstrosity(MonstrositySubtype::Basilisk),
                // The burrowing-ambush archetype.
                quadruped_low::Species::Rocksnapper
                | quadruped_low::Species::Rootsnapper
                | quadruped_low::Species::Reefsnapper => {
                    S::Monstrosity(MonstrositySubtype::Delvemaw)
                },
                // subtipo aproximado, sin match obvio — a lure-and-snare
                // ambush predator; `Mimic` is the only lure-based monster.
                quadruped_low::Species::Snaretongue => S::Monstrosity(MonstrositySubtype::Mimic),
                quadruped_low::Species::Hydra => S::Monstrosity(MonstrositySubtype::Hydra),

                quadruped_low::Species::Lavadrake
                | quadruped_low::Species::Icedrake
                | quadruped_low::Species::Mossdrake => S::Dragon(DragonSubtype::Drake),

                quadruped_low::Species::Maneater => S::Plant(PlantSubtype::VineHorror),
                quadruped_low::Species::Deadwood => S::Plant(PlantSubtype::AwakenedTree),

                quadruped_low::Species::Dagon => S::Aberration(AberrationSubtype::Deepwatcher),

                quadruped_low::Species::Asp => S::Beast(BeastSubtype::Serpent),
                quadruped_low::Species::Monitor => S::Beast(BeastSubtype::Lizard),
                quadruped_low::Species::Tortoise => S::Beast(BeastSubtype::Turtle),
                quadruped_low::Species::Sandshark => S::Beast(BeastSubtype::Fish),
                quadruped_low::Species::Pangolin => S::Beast(BeastSubtype::Mammal),
                // subtipo aproximado, sin match obvio — an engine-original
                // burrowing insectoid.
                quadruped_low::Species::Driggle => S::Beast(BeastSubtype::Insect),
                // Crocodilians, plus `Salamander` — another amphibian filed
                // under `Reptile` for want of a better bucket, as above.
                _ => S::Beast(BeastSubtype::Reptile),
            }),

            Body::BirdMedium(b) => Some(match b.species {
                // subtipo aproximado, sin match obvio — a giant vampiric bat;
                // `Roc` is the giant-flying-monster archetype.
                bird_medium::Species::BloodmoonBat => S::Monstrosity(MonstrositySubtype::Roc),
                // Bats are mammals, not birds, despite the body kind.
                bird_medium::Species::Bat | bird_medium::Species::VampireBat => {
                    S::Beast(BeastSubtype::Mammal)
                },
                _ => S::Beast(BeastSubtype::Avian),
            }),

            Body::BirdLarge(b) => Some(match b.species {
                bird_large::Species::Cockatrice => S::Monstrosity(MonstrositySubtype::Cockatrice),
                bird_large::Species::Roc => S::Monstrosity(MonstrositySubtype::Roc),
                bird_large::Species::Phoenix => S::Celestial(CelestialSubtype::Phoenix),
                _ => S::Dragon(DragonSubtype::Wyvern),
            }),

            Body::FishSmall(_) | Body::FishMedium(_) => Some(S::Beast(BeastSubtype::Fish)),

            Body::BipedLarge(b) => Some(match b.species {
                biped_large::Species::Ogre
                // Japanese oni are conventionally rendered as ogres.
                | biped_large::Species::Blueoni
                | biped_large::Species::Redoni => S::Giant(GiantSubtype::Ogre),
                biped_large::Species::Cyclops => S::Giant(GiantSubtype::Cyclops),
                // subtipo aproximado, sin match obvio — there is no `Troll`
                // subtype; `Ettin` carries the public-domain *eoten*/*jötunn*
                // root that covers trolls, and `Tursus` is literally *þurs*.
                biped_large::Species::Cavetroll
                | biped_large::Species::Mountaintroll
                | biped_large::Species::Swamptroll
                | biped_large::Species::Tursus => S::Giant(GiantSubtype::Ettin),
                biped_large::Species::Gigasfrost => S::Giant(GiantSubtype::FrostGiant),
                biped_large::Species::Gigasfire => S::Giant(GiantSubtype::FireGiant),

                // subtipo aproximado, sin match obvio — an animated scarecrow.
                biped_large::Species::Harvester => S::Construct(ConstructSubtype::AnimatedThing),
                biped_large::Species::HaniwaGeneral
                | biped_large::Species::TerracottaBesieger
                | biped_large::Species::TerracottaDemolisher
                | biped_large::Species::TerracottaPunisher
                | biped_large::Species::TerracottaPursuer => {
                    S::Construct(ConstructSubtype::LivingStatue)
                },
                biped_large::Species::Forgemaster => S::Construct(ConstructSubtype::Golem),

                biped_large::Species::Occultsaurok
                | biped_large::Species::Mightysaurok
                | biped_large::Species::Slysaurok => S::Humanoid(HumanoidSubtype::Lizardfolk),
                // The adlet are dog-people, which is the `Beastblooded`
                // (partial-animal-aspect) bucket.
                biped_large::Species::AdletElder => S::Humanoid(HumanoidSubtype::Beastblooded),
                // subtipo aproximado, sin match obvio — fish-folk; `Triton`
                // (Greek sea-folk) is the only aquatic humanoid lineage here.
                biped_large::Species::SeaBishop => S::Humanoid(HumanoidSubtype::Triton),
                biped_large::Species::Cultistwarlord | biped_large::Species::Cultistwarlock => {
                    S::Humanoid(HumanoidSubtype::Human)
                },

                biped_large::Species::Dullahan => S::Undead(UndeadSubtype::DeathKnight),
                biped_large::Species::Huskbrute => S::Undead(UndeadSubtype::Zombie),
                biped_large::Species::Strigoi => S::Undead(UndeadSubtype::Vampire),
                biped_large::Species::Cursekeeper => S::Undead(UndeadSubtype::Wight),
                biped_large::Species::Executioner => S::Undead(UndeadSubtype::Revenant),

                // subtipo aproximado, sin match obvio — the Algonquian ice
                // cannibal; `Yeti` is the cold-humanoid-monster bucket.
                biped_large::Species::Wendigo => S::Monstrosity(MonstrositySubtype::Yeti),
                // subtipo aproximado, sin match obvio — shapechanging is a
                // *tag*, not a subtype, so the lycanthrope files under the
                // monstrous-wolf subtype.
                biped_large::Species::Werewolf => S::Monstrosity(MonstrositySubtype::Worg),
                // subtipo aproximado, sin match obvio — `Minotaur` exists only
                // as a *Humanoid* subtype, but this species is classified
                // `Monstrosity`; `Chimera` is the nearest composite-beast
                // subtype on that axis.
                biped_large::Species::Minotaur => S::Monstrosity(MonstrositySubtype::Chimera),
                biped_large::Species::Yeti => S::Monstrosity(MonstrositySubtype::Yeti),
                biped_large::Species::Tidalwarrior => {
                    S::Monstrosity(MonstrositySubtype::KrakenSpawn)
                },

                biped_large::Species::Mindflayer => S::Aberration(AberrationSubtype::MindEater),
            }),

            Body::BipedSmall(b) => Some(match b.species {
                biped_small::Species::Gnome => S::Humanoid(HumanoidSubtype::Gnome),
                // subtipo aproximado, sin match obvio — shark-folk; see
                // `SeaBishop` above.
                biped_small::Species::Sahagin => S::Humanoid(HumanoidSubtype::Triton),
                biped_small::Species::Adlet => S::Humanoid(HumanoidSubtype::Beastblooded),
                // subtipo aproximado, sin match obvio — hyena-folk, filed with
                // the other beast-kin for lack of a closer lineage.
                biped_small::Species::Gnoll => S::Humanoid(HumanoidSubtype::Beastblooded),
                biped_small::Species::Gnarling
                | biped_small::Species::GnarlingChieftain
                | biped_small::Species::GoblinThug
                | biped_small::Species::GoblinChucker
                | biped_small::Species::GoblinRuffian => S::Humanoid(HumanoidSubtype::Goblin),

                // subtipo aproximado, sin match obvio — the five `Legoom`
                // vegetable-folk and the walking mandrake/cactus/bush are
                // engine originals with no folkloric analogue.
                biped_small::Species::Mandragora
                | biped_small::Species::Cactid
                | biped_small::Species::Bushly
                | biped_small::Species::Irrwurz
                | biped_small::Species::GreenLegoom
                | biped_small::Species::OchreLegoom
                | biped_small::Species::PurpleLegoom
                | biped_small::Species::RedLegoom
                | biped_small::Species::UmberLegoom => S::Plant(PlantSubtype::LivingPlant),

                biped_small::Species::Husk => S::Undead(UndeadSubtype::Zombie),
                biped_small::Species::Jiangshi | biped_small::Species::BloodmoonHeiress => {
                    S::Undead(UndeadSubtype::Vampire)
                },
                biped_small::Species::ShamanicSpirit => S::Undead(UndeadSubtype::Ghost),
                // The classic vampire-thrall role.
                biped_small::Species::Bloodservant => S::Undead(UndeadSubtype::Ghoul),
                // subtipo aproximado, sin match obvio — a risen castle
                // servant, like the `Executioner`.
                biped_small::Species::Harlequin => S::Undead(UndeadSubtype::Revenant),

                biped_small::Species::Haniwa => S::Construct(ConstructSubtype::LivingStatue),
                biped_small::Species::Myrmidon => S::Construct(ConstructSubtype::AnimatedArmor),
                biped_small::Species::IronDwarf => S::Construct(ConstructSubtype::Clockwork),
                // subtipo aproximado, sin match obvio — a lure-shaped
                // construct chest.
                biped_small::Species::TreasureEgg => S::Construct(ConstructSubtype::AnimatedThing),

                biped_small::Species::Boreal => S::Elemental(ElementalSubtype::Ice),
                biped_small::Species::Ashen => S::Elemental(ElementalSubtype::Ash),
                biped_small::Species::Flamekeeper => S::Elemental(ElementalSubtype::Fire),

                biped_small::Species::Kappa => S::Fey(FeySubtype::NatureSpirit),
            }),

            Body::Theropod(b) => Some(match b.species {
                // subtipo aproximado, sin match obvio — the yale is a heraldic
                // composite beast; `Chimera` is the nearest.
                theropod::Species::Yale => S::Monstrosity(MonstrositySubtype::Chimera),
                _ => S::Beast(BeastSubtype::Dinosaur),
            }),

            Body::Arthropod(b) => Some(match b.species {
                // A burrowing fire-worm, which is exactly the `Emberworm`
                // archetype.
                arthropod::Species::Moltencrawler => S::Monstrosity(MonstrositySubtype::Emberworm),
                arthropod::Species::Mosscrawler | arthropod::Species::Sandcrawler => {
                    S::Monstrosity(MonstrositySubtype::Delvemaw)
                },
                // Same abyssal lineage as `QuadrupedLow::Dagon`.
                arthropod::Species::Dagonite => S::Aberration(AberrationSubtype::Deepwatcher),
                arthropod::Species::Tarantula
                | arthropod::Species::Blackwidow
                | arthropod::Species::Cavespider => S::Beast(BeastSubtype::Spider),
                _ => S::Beast(BeastSubtype::Insect),
            }),

            Body::Crustacean(b) => Some(match b.species {
                crustacean::Species::Crab | crustacean::Species::SoldierCrab => {
                    S::Beast(BeastSubtype::Crustacean)
                },
                crustacean::Species::Karkatha => S::Monstrosity(MonstrositySubtype::KrakenSpawn),
            }),

            Body::Dragon(_) => Some(S::Dragon(DragonSubtype::Chromatic)),

            // Mirrors the `creature_kind` arm exactly — DO NOT collapse, for
            // the same reason given there.
            Body::Golem(b) => Some(match b.species {
                golem::Species::StoneGolem
                | golem::Species::ClayGolem
                | golem::Species::WoodGolem
                | golem::Species::CoralGolem
                | golem::Species::IronGolem => S::Construct(ConstructSubtype::Golem),
                golem::Species::AncientEffigy | golem::Species::Gravewarden => {
                    S::Construct(ConstructSubtype::LivingStatue)
                },
                golem::Species::Treant => S::Plant(PlantSubtype::Treant),
                golem::Species::Mogwai => S::Fiend(FiendSubtype::Demon),
            }),

            // The same combat-capable allowlist as `creature_kind`: every
            // entry there appears here, and `TrainingDummy` in neither.
            Body::Object(o) => match o {
                object::Body::TerracottaStatue | object::Body::HaniwaSentry => {
                    Some(S::Construct(ConstructSubtype::LivingStatue))
                },
                object::Body::ArrowTurret | object::Body::BarrelOrgan | object::Body::Crux => {
                    Some(S::Construct(ConstructSubtype::MagicalMachine))
                },
                object::Body::GnarlingTotemRed
                | object::Body::GnarlingTotemGreen
                | object::Body::GnarlingTotemWhite => {
                    Some(S::Construct(ConstructSubtype::RuneConstruct))
                },
                _ => None,
            },

            Body::Item(_) | Body::Ship(_) | Body::Plugin(_) => None,
        }
    }

    /// The layer-4 tags this body carries by default. Empty is a perfectly
    /// valid answer and the common case.
    ///
    /// ⚪ **Only the structurally-derivable tags live here.** `FLYING` and
    /// `AQUATIC` describe where a body physically operates, which is a
    /// property of the body itself and so belongs with the body defaults. The
    /// other 32 tags (`ANCIENT`, `ELITE`, `NAMED`, `ROYAL`, `UNIQUE`, …)
    /// describe a particular *spawn*, not a species, and are deliberately not
    /// hardcoded per-species: they belong to the entity-config authoring
    /// layer, which is specified to union with whatever this method returns
    /// rather than replace it.
    pub fn creature_tags(&self) -> CreatureTags {
        let mut tags = CreatureTags::empty();

        // Flight follows the engine's own capability (`Body::fly_thrust`):
        // every bird body, every true dragon, and the Crux.
        if matches!(
            self,
            Body::BirdMedium(_)
                | Body::BirdLarge(_)
                | Body::Dragon(_)
                | Body::Object(object::Body::Crux)
        ) {
            tags |= CreatureTags::FLYING;
        }

        let aquatic = match self {
            Body::FishSmall(_) | Body::FishMedium(_) | Body::Crustacean(_) => true,
            Body::QuadrupedSmall(b) => matches!(
                b.species,
                quadruped_small::Species::Axolotl
                    | quadruped_small::Species::Frog
                    | quadruped_small::Species::Seal
            ),
            Body::QuadrupedMedium(b) => matches!(
                b.species,
                quadruped_medium::Species::Akhlut
                    | quadruped_medium::Species::Roshwalr
                    | quadruped_medium::Species::Kelpie
            ),
            Body::QuadrupedLow(b) => matches!(
                b.species,
                quadruped_low::Species::Crocodile
                    | quadruped_low::Species::Alligator
                    | quadruped_low::Species::SeaCrocodile
                    | quadruped_low::Species::Sandshark
                    | quadruped_low::Species::Hakulaq
                    | quadruped_low::Species::Elbst
                    | quadruped_low::Species::Reefsnapper
                    | quadruped_low::Species::Dagon
            ),
            Body::BirdMedium(b) => matches!(b.species, bird_medium::Species::Penguin),
            Body::BirdLarge(b) => matches!(b.species, bird_large::Species::SeaWyvern),
            Body::BipedSmall(b) => matches!(
                b.species,
                biped_small::Species::Sahagin | biped_small::Species::Kappa
            ),
            Body::BipedLarge(b) => matches!(
                b.species,
                biped_large::Species::Tidalwarrior | biped_large::Species::SeaBishop
            ),
            _ => false,
        };
        if aquatic {
            tags |= CreatureTags::AQUATIC;
        }

        tags
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::comp::body::{
        biped_large, biped_small, fish_small, golem, humanoid, object, quadruped_medium,
    };

    fn humanoid_body(species: humanoid::Species) -> Body {
        Body::Humanoid(humanoid::Body {
            species,
            body_type: humanoid::BodyType::Male,
            hair_style: 0,
            beard: 0,
            eyes: 0,
            accessory: 0,
            hair_color: 0,
            skin: 0,
            eye_color: 0,
            height_scale: 0,
        })
    }

    fn biped_small_body(species: biped_small::Species) -> Body {
        Body::BipedSmall(biped_small::Body {
            species,
            body_type: biped_small::BodyType::Male,
        })
    }

    fn biped_large_body(species: biped_large::Species) -> Body {
        Body::BipedLarge(biped_large::Body {
            species,
            body_type: biped_large::BodyType::Male,
        })
    }

    fn quadruped_medium_body(species: quadruped_medium::Species) -> Body {
        Body::QuadrupedMedium(quadruped_medium::Body {
            species,
            body_type: quadruped_medium::BodyType::Male,
        })
    }

    fn golem_body(species: golem::Species) -> Body {
        Body::Golem(golem::Body {
            species,
            body_type: golem::BodyType::Male,
        })
    }

    /// The chief acceptance criterion: every creature-bearing `Body` kind
    /// must classify every one of its species. `Body::iter()` (from
    /// `enum_iter!`) walks every nested species, so a forgotten arm is a
    /// test failure here, never a silent `None`.
    #[test]
    fn every_creature_body_has_a_creature_kind() {
        for body in Body::iter() {
            match body {
                Body::Object(_) | Body::Item(_) | Body::Ship(_) | Body::Plugin(_) => {},
                _ => assert!(
                    body.creature_kind().is_some(),
                    "unclassified creature body: {body:?}"
                ),
            }
        }
    }

    /// The layer-3 sibling of the above: every body that has a kind must also
    /// have a subtype.
    #[test]
    fn every_creature_body_has_a_creature_subtype() {
        for body in Body::iter() {
            match body {
                Body::Object(_) | Body::Item(_) | Body::Ship(_) | Body::Plugin(_) => {},
                _ => assert!(
                    body.creature_subtype().is_some(),
                    "creature body with no subtype: {body:?}"
                ),
            }
        }
    }

    /// 🔴 **The most important test in this module.** Splitting the taxonomy
    /// into three independent types (rather than nesting the subtype inside
    /// `CreatureKind`) buys `[f32; NUM_KINDS]` at the cost of making it
    /// *possible* to file a species under a subtype of the wrong kind —
    /// something the compiler cannot see. This is the only safety net.
    #[test]
    fn subtype_and_kind_never_contradict_each_other() {
        for body in Body::iter() {
            assert_eq!(
                body.creature_subtype().map(CreatureSubtype::kind),
                body.creature_kind(),
                "kind/subtype disagree for {body:?}: {:?} vs {:?}",
                body.creature_subtype(),
                body.creature_kind(),
            );
        }
    }

    /// `Object::TrainingDummy` is the one object deliberately exempt from
    /// `Charmed` immunity (`Body::immune_to`) — it must never become a
    /// `Construct`, on either layer.
    #[test]
    fn training_dummy_is_not_a_creature() {
        let dummy = Body::Object(object::Body::TrainingDummy);
        assert_eq!(dummy.creature_kind(), None);
        assert_eq!(dummy.creature_subtype(), None);
    }

    #[test]
    fn ordinary_objects_are_not_creatures() {
        let arrow = Body::Object(object::Body::Arrow);
        assert_eq!(arrow.creature_kind(), None);
        assert_eq!(arrow.creature_subtype(), None);
    }

    /// The `Object` combat-capable allowlist — identical on both layers.
    #[test]
    fn combat_capable_objects_are_constructs() {
        for o in [
            object::Body::TerracottaStatue,
            object::Body::HaniwaSentry,
            object::Body::ArrowTurret,
            object::Body::BarrelOrgan,
            object::Body::GnarlingTotemRed,
            object::Body::GnarlingTotemGreen,
            object::Body::GnarlingTotemWhite,
            object::Body::Crux,
        ] {
            let body = Body::Object(o);
            assert_eq!(
                body.creature_kind(),
                Some(CreatureKind::Construct),
                "{o:?} should be Construct"
            );
            assert_eq!(
                body.creature_subtype().map(CreatureSubtype::kind),
                Some(CreatureKind::Construct),
                "{o:?} subtype should be on the Construct axis"
            );
        }
    }

    /// The 12 species that are Undead — the vampire-castle roster plus the
    /// terracotta-tomb spirit/boss.
    #[test]
    fn the_twelve_undead_species_are_undead() {
        assert_eq!(
            quadruped_medium_body(quadruped_medium::Species::Bonerattler).creature_kind(),
            Some(CreatureKind::Undead)
        );

        for species in [
            biped_large::Species::Dullahan,
            biped_large::Species::Huskbrute,
            biped_large::Species::Strigoi,
            biped_large::Species::Cursekeeper,
            biped_large::Species::Executioner,
        ] {
            assert_eq!(
                biped_large_body(species).creature_kind(),
                Some(CreatureKind::Undead),
                "{species:?} should be Undead"
            );
        }

        for species in [
            biped_small::Species::Husk,
            biped_small::Species::Jiangshi,
            biped_small::Species::ShamanicSpirit,
            biped_small::Species::BloodmoonHeiress,
            biped_small::Species::Bloodservant,
            biped_small::Species::Harlequin,
        ] {
            assert_eq!(
                biped_small_body(species).creature_kind(),
                Some(CreatureKind::Undead),
                "{species:?} should be Undead"
            );
        }
    }

    /// The three shipped vampires land on the same subtype, which is what
    /// makes an anti-vampire predicate expressible at layer 3 instead of
    /// needing a fourth enum.
    #[test]
    fn the_three_vampires_share_a_subtype() {
        for body in [
            biped_large_body(biped_large::Species::Strigoi),
            biped_small_body(biped_small::Species::Jiangshi),
            biped_small_body(biped_small::Species::BloodmoonHeiress),
        ] {
            assert_eq!(
                body.creature_subtype(),
                Some(CreatureSubtype::Undead(UndeadSubtype::Vampire)),
                "{body:?} should be an Undead(Vampire)"
            );
        }
    }

    /// `Body::Golem(_)` is NOT `Construct` in block — 2 of 9 Golem species
    /// are not, on both layers.
    #[test]
    fn golem_treant_is_plant_and_mogwai_is_fiend() {
        let treant = golem_body(golem::Species::Treant);
        assert_eq!(treant.creature_kind(), Some(CreatureKind::Plant));
        assert_eq!(
            treant.creature_subtype(),
            Some(CreatureSubtype::Plant(PlantSubtype::Treant))
        );

        let mogwai = golem_body(golem::Species::Mogwai);
        assert_eq!(mogwai.creature_kind(), Some(CreatureKind::Fiend));
        assert_eq!(
            mogwai.creature_subtype(),
            Some(CreatureSubtype::Fiend(FiendSubtype::Demon))
        );

        // And an ordinary Golem species stays Construct.
        assert_eq!(
            golem_body(golem::Species::StoneGolem).creature_kind(),
            Some(CreatureKind::Construct)
        );
    }

    /// `Body::Humanoid(_)` (body shape) and `CreatureKind::Humanoid`
    /// (creature kind) are independent axes.
    #[test]
    fn body_humanoid_shape_and_creature_kind_humanoid_are_independent_axes() {
        // A BipedSmall(Gnome) is a Humanoid creature without being a
        // `Body::Humanoid`.
        assert_eq!(
            biped_small_body(biped_small::Species::Gnome).creature_kind(),
            Some(CreatureKind::Humanoid)
        );
        assert!(!matches!(
            biped_small_body(biped_small::Species::Gnome),
            Body::Humanoid(_)
        ));

        // Body::Humanoid(Draugr) IS `Body::Humanoid` (body shape) AND is
        // CreatureKind::Humanoid — a Draugr is a living lineage, not
        // Undead, despite the name.
        let draugr = humanoid_body(humanoid::Species::Draugr);
        assert!(matches!(draugr, Body::Humanoid(_)));
        assert_eq!(draugr.creature_kind(), Some(CreatureKind::Humanoid));
    }

    /// `CreatureSubtype::kind()` is total and correct for every subtype of
    /// every kind, not just the ones a shipped species happens to use.
    #[test]
    fn every_subtype_variant_reports_its_own_kind() {
        for subtype in CreatureSubtype::iter() {
            let expected = match subtype {
                CreatureSubtype::Aberration(_) => CreatureKind::Aberration,
                CreatureSubtype::Beast(_) => CreatureKind::Beast,
                CreatureSubtype::Celestial(_) => CreatureKind::Celestial,
                CreatureSubtype::Construct(_) => CreatureKind::Construct,
                CreatureSubtype::Dragon(_) => CreatureKind::Dragon,
                CreatureSubtype::Elemental(_) => CreatureKind::Elemental,
                CreatureSubtype::Fey(_) => CreatureKind::Fey,
                CreatureSubtype::Fiend(_) => CreatureKind::Fiend,
                CreatureSubtype::Giant(_) => CreatureKind::Giant,
                CreatureSubtype::Humanoid(_) => CreatureKind::Humanoid,
                CreatureSubtype::Monstrosity(_) => CreatureKind::Monstrosity,
                CreatureSubtype::Ooze(_) => CreatureKind::Ooze,
                CreatureSubtype::Plant(_) => CreatureKind::Plant,
                CreatureSubtype::Undead(_) => CreatureKind::Undead,
            };
            assert_eq!(
                subtype.kind(),
                expected,
                "{subtype:?} reports the wrong kind"
            );
        }
    }

    /// Every kind has at least one subtype, so `creature_subtype()` can never
    /// be blocked by an empty enum.
    #[test]
    fn every_kind_has_at_least_one_subtype() {
        for kind in CreatureKind::ALL {
            assert!(
                CreatureSubtype::iter().any(|s| s.kind() == kind),
                "{kind:?} has no subtypes at all"
            );
        }
    }

    /// The tags must all be distinct bits and fit in the `u64`.
    #[test]
    fn every_tag_is_a_distinct_bit() {
        assert_eq!(
            CreatureTags::all().bits().count_ones(),
            34,
            "expected 34 distinct tag bits"
        );
    }

    /// Layer 4 is body-derived and empty by default for most bodies.
    #[test]
    fn structural_tags_are_body_derived() {
        let clownfish = Body::FishSmall(fish_small::Body {
            species: fish_small::Species::Clownfish,
            body_type: fish_small::BodyType::Female,
        });
        assert!(clownfish.creature_tags().contains(CreatureTags::AQUATIC));
        assert!(!clownfish.creature_tags().contains(CreatureTags::FLYING));

        let crux = Body::Object(object::Body::Crux);
        assert!(crux.creature_tags().contains(CreatureTags::FLYING));
        assert!(!crux.creature_tags().contains(CreatureTags::AQUATIC));

        assert!(
            golem_body(golem::Species::StoneGolem)
                .creature_tags()
                .is_empty()
        );
    }
}
