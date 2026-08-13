use crate::comp::{
    CreatureKind, Stats,
    skillset::{
        SKILL_GROUP_LOOKUP, SKILL_MAX_LEVEL, SKILL_PREREQUISITES, SkillGroupKind, SkillPrerequisite,
    },
};
use serde::{Deserialize, Serialize};

/// Represents a skill that a player can unlock, that either grants them some
/// kind of active ability, or a passive effect etc. Obviously because this is
/// an enum it doesn't describe what the skill actually -does-, this will be
/// handled by dedicated ECS systems.
// NOTE: if skill does use some constant, add it to corresponding
// SkillTree Modifiers below.
#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq, Serialize, Deserialize, Ord, PartialOrd)]
pub enum Skill {
    Sword(SwordSkill),
    Axe(AxeSkill),
    Hammer(HammerSkill),
    Bow(BowSkill),
    Staff(StaffSkill),
    Sceptre(SceptreSkill),
    /// The martial-role Staff's own skill tree, kept separate from
    /// `Staff(StaffSkill)` (the caster/fire tree) because the two trees
    /// share a `ToolKind` but not a `WeaponRole`.
    StaffMartial(StaffMartialSkill),
    Climb(ClimbSkill),
    Swim(SwimSkill),
    Pick(MiningSkill),
    // BL-06 class skill trees. Variants are mostly passive stat skills (their
    // per-level stat modifiers live in the `class_skill_modifiers.ron` manifest,
    // applied generically in the buff system) plus a couple of signature
    // active-ability unlocks per class (gated like weapon abilities).
    Warrior(WarriorSkill),
    Mage(MageSkill),
    Cleric(ClericSkill),
    Rogue(RogueSkill),
    /// The Cadena pact-boon's investment track. `max_level: 5`, +1 summon
    /// point-pool per rank (`comp::pact::chain_pool`) -- not a `Stats`
    /// passive, so it carries no per-level stat-modifier manifest entry.
    Warlock(WarlockSkill),
    // BL-20 feats/skills system. A single class-agnostic group (see
    // `SkillGroupKind::Feats`); `FeatSkill` carries the V1-implementable feat
    // subset locked in `docs/design/plans/2026-07-01-feats-p0-triage.md`.
    Feat(FeatSkill),
    UnlockGroup(SkillGroupKind),
}

#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq, Serialize, Deserialize, Ord, PartialOrd)]
pub enum WarlockSkill {
    /// Ranked 1-5. Each rank adds one point to the Cadena boon's summon
    /// point pool (`comp::pact::chain_pool`).
    ChainMastery,
}

#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq, Serialize, Deserialize, Ord, PartialOrd)]
pub enum WarriorSkill {
    // T1
    HardenedBody,
    PracticedStrikes,
    Rally, // ACTIVE (signature)
    // T2
    IronSkin,
    BrutalEdge,
    CrushingBlows,
    Stalwart,
    SunderingForce,
    Stagger,
    BattleMomentum,
    // T3
    BulwarkStance, // notable
    Onslaught,     // ACTIVE (capstone, synergy <- BrutalEdge)
}

#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq, Serialize, Deserialize, Ord, PartialOrd)]
pub enum MageSkill {
    // T1
    FocusedMind,
    TrueAim,
    ArcaneSurge, // ACTIVE (signature)
    // T2
    SpellPotency,
    PyromanticAttunement,
    CryomanticAttunement,
    QuickCasting,
    PenetratingMagic,
    WardedSkin,
    ManaEfficiency,
    ManaRecover,
    ManaFlow,
    ArcaneVigor,
    Polyglot,
    // T3
    Overcharge,    // notable
    ArcaneMastery, // ACTIVE (capstone, synergy <- FocusedMind)
}

#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq, Serialize, Deserialize, Ord, PartialOrd)]
pub enum ClericSkill {
    // T1
    FaithfulVigor,
    DevoutFocus,
    MendingLight, // ACTIVE (signature)
    // T2
    BlessedAim,
    SacredWards,
    SteadfastFaith,
    PurifyingGrace,
    DivineConduit,
    SmitingStrikes,
    ArmorOfFaith,
    // T3
    Aegis,          // notable
    RadiantChannel, // ACTIVE (capstone)
}

#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq, Serialize, Deserialize, Ord, PartialOrd)]
pub enum RogueSkill {
    // T1
    Lithe,
    KeenEdge,
    Ambush, // ACTIVE (signature)
    // T2
    DeadlyPrecision,
    FleetFooted,
    SureStrike,
    FindTheGap,
    QuickHands,
    ToxinTolerance,
    Opportunist,
    // T3
    Shadowstep, // notable
    Vanish,     // ACTIVE (capstone, synergy <- DeadlyPrecision)
}

// BL-20 feats/skills system. One variant per V1-implementable feat (PASSIVE +
// ACTIVE only), locked in `docs/design/plans/2026-07-01-feats-p0-triage.md`.
// DEFERRED feats and Epic Boons are NOT represented here (see the design doc
// §4 / triage doc "DEFERRED" rows). All feats are max_level = 1 (binary
// purchase), so no explicit `skill_max_levels.ron` entry is needed — the
// default (1) already matches.
#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq, Serialize, Deserialize, Ord, PartialOrd)]
pub enum FeatSkill {
    // ---------- Combat ----------
    Athlete,
    Charger,
    Crusher,
    CrossbowExpert,
    DefensiveDuelist,
    DualWielder,
    GreatWeaponMaster,
    HeavyArmorMaster,
    MageSlayer,
    Mobile,
    Piercer,
    PolearmMaster,
    SavageAttacker,
    Sentinel,
    Sharpshooter,
    ShieldMaster,
    Slasher,
    Speedy,
    TavernBrawler,
    // ---------- Magic ----------
    AberrantBloodmark,
    ArcaneCollegeInitiate,
    ArtificerInitiate,
    ElementalAdept,
    FrostCaster,
    GenieMagic,
    GiftOfTheChromaticDragon,
    GiftOfTheGemDragon,
    GiftOfTheMetallicDragon,
    GreaterAberrantBloodmark,
    MagicInitiate,
    MythalTouched,
    SpellSniper,
    SpellfireAdept,
    SpellfireSpark,
    Telekinetic,
    Telepathic,
    UmbraTouched,
    VeilTouched,
    WarCaster,
    // ---------- Social ----------
    FairyTrickster,
    InspiringLeader,
    LordlyResolve,
    TirelessReveler,
    // ---------- Exploration ----------
    Alert,
    Chef,
    ChildOfTheSun,
    DungeonDelver,
    Healer,
    Observant,
    ShadowmoorHexer,
    // ---------- Craft ----------
    Bombardier,
    DraconicCultInitiate,
    Dragonscarred,
    OrdersResilience,
    Poisoner,
    Quicksmith,
    StrikeOfTheGiants,
    VampireHunter,
    // ---------- Fate ----------
    Bloodlust,
    CloyingMists,
    DeliciousPain,
    Durable,
    LightBringer,
    LoveBites,
    Lucky,
    Putrefy,
    Rebuke,
    Resilient,
    Tough,
    TreacherousAllure,
    VampireTouched,
    VampiresPlaything,
}

/// The six lore pillars (Combat / Magic / Social / Exploration / Craft /
/// Fate) a feat belongs to. UI category tag only (design doc §1.3) — used to
/// group feats into visual sections in the diary Feats tab; not a separate
/// skill group, manifest, or XP pool.
#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq, Serialize, Deserialize, Ord, PartialOrd)]
pub enum FeatPillar {
    Combat,
    Magic,
    Social,
    Exploration,
    Craft,
    Fate,
}

impl FeatSkill {
    pub const fn pillar(&self) -> FeatPillar {
        match self {
            FeatSkill::Athlete
            | FeatSkill::Charger
            | FeatSkill::Crusher
            | FeatSkill::CrossbowExpert
            | FeatSkill::DefensiveDuelist
            | FeatSkill::DualWielder
            | FeatSkill::GreatWeaponMaster
            | FeatSkill::HeavyArmorMaster
            | FeatSkill::MageSlayer
            | FeatSkill::Mobile
            | FeatSkill::Piercer
            | FeatSkill::PolearmMaster
            | FeatSkill::SavageAttacker
            | FeatSkill::Sentinel
            | FeatSkill::Sharpshooter
            | FeatSkill::ShieldMaster
            | FeatSkill::Slasher
            | FeatSkill::Speedy
            | FeatSkill::TavernBrawler => FeatPillar::Combat,

            FeatSkill::AberrantBloodmark
            | FeatSkill::ArcaneCollegeInitiate
            | FeatSkill::ArtificerInitiate
            | FeatSkill::ElementalAdept
            | FeatSkill::FrostCaster
            | FeatSkill::GenieMagic
            | FeatSkill::GiftOfTheChromaticDragon
            | FeatSkill::GiftOfTheGemDragon
            | FeatSkill::GiftOfTheMetallicDragon
            | FeatSkill::GreaterAberrantBloodmark
            | FeatSkill::MagicInitiate
            | FeatSkill::MythalTouched
            | FeatSkill::SpellSniper
            | FeatSkill::SpellfireAdept
            | FeatSkill::SpellfireSpark
            | FeatSkill::Telekinetic
            | FeatSkill::Telepathic
            | FeatSkill::UmbraTouched
            | FeatSkill::VeilTouched
            | FeatSkill::WarCaster => FeatPillar::Magic,

            FeatSkill::FairyTrickster
            | FeatSkill::InspiringLeader
            | FeatSkill::LordlyResolve
            | FeatSkill::TirelessReveler => FeatPillar::Social,

            FeatSkill::Alert
            | FeatSkill::Chef
            | FeatSkill::ChildOfTheSun
            | FeatSkill::DungeonDelver
            | FeatSkill::Healer
            | FeatSkill::Observant
            | FeatSkill::ShadowmoorHexer => FeatPillar::Exploration,

            FeatSkill::Bombardier
            | FeatSkill::DraconicCultInitiate
            | FeatSkill::Dragonscarred
            | FeatSkill::OrdersResilience
            | FeatSkill::Poisoner
            | FeatSkill::Quicksmith
            | FeatSkill::StrikeOfTheGiants
            | FeatSkill::VampireHunter => FeatPillar::Craft,

            FeatSkill::Bloodlust
            | FeatSkill::CloyingMists
            | FeatSkill::DeliciousPain
            | FeatSkill::Durable
            | FeatSkill::LightBringer
            | FeatSkill::LoveBites
            | FeatSkill::Lucky
            | FeatSkill::Putrefy
            | FeatSkill::Rebuke
            | FeatSkill::Resilient
            | FeatSkill::Tough
            | FeatSkill::TreacherousAllure
            | FeatSkill::VampireTouched
            | FeatSkill::VampiresPlaything => FeatPillar::Fate,
        }
    }
}

#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq, Serialize, Deserialize, Ord, PartialOrd)]
pub enum SwordSkill {
    CrescentSlash,
    FellStrike,
    Skewer,
    Cascade,
    CrossCut,
    Finisher,
    HeavySweep,
    HeavyPommelStrike,
    HeavyFortitude,
    HeavyPillarThrust,
    AgileQuickDraw,
    AgileFeint,
    AgileDancingEdge,
    AgileFlurry,
    DefensiveRiposte,
    DefensiveDisengage,
    DefensiveDeflect,
    DefensiveStalwartSword,
    CripplingGouge,
    CripplingHamstring,
    CripplingBloodyGash,
    CripplingEviscerate,
    CleavingWhirlwindSlice,
    CleavingEarthSplitter,
    CleavingSkySplitter,
    CleavingBladeFever,
}

#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq, Serialize, Deserialize, Ord, PartialOrd)]
pub enum AxeSkill {
    BrutalSwing,
    Berserk,
    RisingTide,
    SavageSense,
    AdrenalineRush,
    Execute,
    Maelstrom,
    Rake,
    Bloodfeast,
    FierceRaze,
    Furor,
    Fracture,
    Lacerate,
    Riptide,
    SkullBash,
    Sunder,
    Plunder,
    Defiance,
    Keelhaul,
    Bulkhead,
    Capsize,
}

#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq, Serialize, Deserialize, Ord, PartialOrd)]
pub enum HammerSkill {
    ScornfulSwipe,
    Tremor,
    VigorousBash,
    Retaliate,
    SpineCracker,
    Breach,
    IronTempest,
    Upheaval,
    Thunderclap,
    SeismicShock,
    HeavyWhorl,
    Intercept,
    PileDriver,
    LungPummel,
    HelmCrusher,
    Rampart,
    Tenacity,
    Earthshaker,
    Judgement,
}

#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq, Serialize, Deserialize, Ord, PartialOrd)]
pub enum BowSkill {
    Foothold,
    HeavyNock,
    ArdentHunt,
    StormChaser,
    EagleEye,
    Heartseeker,
    Hawkstrike,
    SepticShot,
    IgniteArrow,
    DrenchArrow,
    FreezeArrow,
    JoltArrow,
    Barrage,
    PiercingGale,
    ThornStake,
    Fusillade,
    DeathVolley,
}

#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq, Serialize, Deserialize, Ord, PartialOrd)]
pub enum StaffSkill {
    FireShockwave,
    NapalmStrike,
    FlameCloak,
    FireDash,
    FireBreath,
    Pyroclasm,
}

/// A small, coherent starter tree for the martial (physical) Staff kit: two
/// T1 roots (crowd-control `Sweep` vs. single-target `Brace`), each with a
/// T2 follow-up, converging on a T3 capstone that requires both T2s. All
/// nodes gate an active ability (see `ability_set_manifest.ron`'s
/// `Custom("staff_martial")` entry); none carry a passive stat modifier.
#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq, Serialize, Deserialize, Ord, PartialOrd)]
pub enum StaffMartialSkill {
    Sweep,
    Brace,
    WhirlingGale,
    GlacialThrust,
    Avalanche,
}

#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq, Serialize, Deserialize, Ord, PartialOrd)]
pub enum SceptreSkill {
    // Lifesteal beam upgrades
    LDamage,
    LRange,
    LLifesteal,
    LRegen,
    // Healing aura upgrades
    HHeal,
    HRange,
    HDuration,
    HCost,
    // Warding aura upgrades
    UnlockAura,
    AStrength,
    ADuration,
    ARange,
    ACost,
}

#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq, Serialize, Deserialize, Ord, PartialOrd)]
pub enum ClimbSkill {
    Cost,
    Speed,
}

#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq, Serialize, Deserialize, Ord, PartialOrd)]
pub enum SwimSkill {
    Speed,
}

#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq, Serialize, Deserialize, Ord, PartialOrd)]
pub enum MiningSkill {
    Speed,
    OreGain,
    GemGain,
}

impl Skill {
    /// Is unable to detect cyclic dependencies, so ensure that there are no
    /// cycles if you modify the prerequisite map.
    pub fn prerequisite_skills(&self) -> Option<&SkillPrerequisite> {
        SKILL_PREREQUISITES.get(self)
    }

    /// Returns the cost in skill points of unlocking a particular skill
    pub fn skill_cost(&self, level: u16) -> u16 { level }

    /// Returns the maximum level a skill can reach, returns None if the skill
    /// doesn't level
    pub fn max_level(&self) -> u16 { SKILL_MAX_LEVEL.get(self).copied().unwrap_or(1) }

    /// Returns the skill group type for a skill from the static skill group
    /// definitions.
    pub fn skill_group_kind(&self) -> Option<SkillGroupKind> {
        SKILL_GROUP_LOOKUP.get(self).copied()
    }
}

/// Tree of modifiers that represent how stats are
/// changed per each skill level.
///
/// It's used as bridge between ECS systems
/// and voxygen Diary for skill descriptions and helps to sync them.
///
/// NOTE: Just adding constant does nothing, you need to use it in both
/// ECS systems and Diary.
// TODO: make it lazy_static and move to .ron?
pub const SKILL_MODIFIERS: SkillTreeModifiers = SkillTreeModifiers::get();

pub struct SkillTreeModifiers {
    pub staff_tree: StaffTreeModifiers,
    pub sceptre_tree: SceptreTreeModifiers,
    pub mining_tree: MiningTreeModifiers,
    pub general_tree: GeneralTreeModifiers,
}

impl SkillTreeModifiers {
    const fn get() -> Self {
        Self {
            staff_tree: StaffTreeModifiers::get(),
            sceptre_tree: SceptreTreeModifiers::get(),
            mining_tree: MiningTreeModifiers::get(),
            general_tree: GeneralTreeModifiers::get(),
        }
    }
}

pub struct StaffTreeModifiers {
    pub fireball: StaffFireballModifiers,
    pub flamethrower: StaffFlamethrowerModifiers,
    pub shockwave: StaffShockwaveModifiers,
}

pub struct StaffFireballModifiers {
    pub power: f32,
    pub regen: f32,
    pub range: f32,
}

pub struct StaffFlamethrowerModifiers {
    pub damage: f32,
    pub range: f32,
    pub energy_drain: f32,
    pub velocity: f32,
}

pub struct StaffShockwaveModifiers {
    pub damage: f32,
    pub knockback: f32,
    pub duration: f32,
    pub energy_cost: f32,
}

impl StaffTreeModifiers {
    const fn get() -> Self {
        Self {
            fireball: StaffFireballModifiers {
                power: 1.05,
                regen: 1.05,
                range: 1.05,
            },
            flamethrower: StaffFlamethrowerModifiers {
                damage: 1.1,
                range: 1.05,
                energy_drain: 0.95,
                velocity: 1.05,
            },
            shockwave: StaffShockwaveModifiers {
                damage: 1.1,
                knockback: 1.05,
                duration: 1.05,
                energy_cost: 0.95,
            },
        }
    }
}

pub struct SceptreTreeModifiers {
    pub beam: SceptreBeamModifiers,
    pub healing_aura: SceptreHealingAuraModifiers,
    pub warding_aura: SceptreWardingAuraModifiers,
}

pub struct SceptreBeamModifiers {
    pub damage: f32,
    pub range: f32,
    pub energy_regen: f32,
    pub lifesteal: f32,
}

pub struct SceptreHealingAuraModifiers {
    pub strength: f32,
    pub duration: f32,
    pub range: f32,
    pub energy_cost: f32,
}

pub struct SceptreWardingAuraModifiers {
    pub strength: f32,
    pub duration: f32,
    pub range: f32,
    pub energy_cost: f32,
}

impl SceptreTreeModifiers {
    const fn get() -> Self {
        Self {
            beam: SceptreBeamModifiers {
                damage: 1.05,
                range: 1.05,
                energy_regen: 1.05,
                lifesteal: 1.05,
            },
            healing_aura: SceptreHealingAuraModifiers {
                strength: 1.05,
                duration: 1.05,
                range: 1.05,
                energy_cost: 0.95,
            },
            warding_aura: SceptreWardingAuraModifiers {
                strength: 1.05,
                duration: 1.05,
                range: 1.05,
                energy_cost: 0.95,
            },
        }
    }
}

pub struct MiningTreeModifiers {
    pub speed: f32,
    pub gem_gain: f32,
    pub ore_gain: f32,
}

impl MiningTreeModifiers {
    const fn get() -> Self {
        Self {
            speed: 1.1,
            gem_gain: 0.1,
            ore_gain: 0.1,
        }
    }
}

pub struct GeneralTreeModifiers {
    pub swim: SwimTreeModifiers,
    pub climb: ClimbTreeModifiers,
}

pub struct SwimTreeModifiers {
    pub speed: f32,
}

pub struct ClimbTreeModifiers {
    pub energy_cost: f32,
    pub speed: f32,
}

impl GeneralTreeModifiers {
    const fn get() -> Self {
        Self {
            swim: SwimTreeModifiers { speed: 1.25 },
            climb: ClimbTreeModifiers {
                energy_cost: 0.8,
                speed: 1.2,
            },
        }
    }
}

/// A `Stats` field a passive class skill (BL-06) can boost. The per-level
/// magnitudes live in `class_skill_modifiers.ron`; the buff system folds
/// `magnitude * skill_level` into the matching field each tick (after the
/// reset), via [`ClassPassiveStat::apply`]. Adding a variant requires adding a
/// match arm here AND (to take effect) a manifest entry that references it.
#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq, Serialize, Deserialize)]
pub enum ClassPassiveStat {
    MaxHealth,
    MaxEnergy,
    AttackDamage,
    /// BL-06 (Q2) magic-source damage channel (gated to spells in
    /// `apply_attack`).
    SpellPower,
    /// BL-06 (Q2) healing-output channel.
    HealPower,
    Accuracy,
    Evasion,
    MagicAccuracy,
    CritChance,
    PrecisionMult,
    ResistFire,
    ResistFrost,
    ResistPoison,
    ResistMagic,
    CrowdControlResistance,
    DamageReduction,
    MitigationsPenetration,
    PoiseDamage,
    MoveSpeed,
    RecoverySpeed,
    EnergyReward,
    /// Energy *cost* reduction (a divisor, see [`ClassPassiveStat::apply`]'s
    /// match arm below) — distinct from `EnergyReward` (energy *gained back*
    /// on hit). Reaches `Stats::energy_efficiency_modifier`, already consumed
    /// at `states/utils.rs` and `ability.rs`'s `*energy_cost /=
    /// stats.energy_efficiency` sites.
    EnergyEfficiency,
    /// Energy regeneration *rate* multiplier. Reaches
    /// `Stats::energy_regen_modifier`, consumed by both `Energy::regen(..)`
    /// call sites in `common/systems/src/stats.rs`.
    EnergyRegen,
    /// Extra damage vs targets of a given `CreatureKind` (the Cleric smite is
    /// `BonusVs(Undead)`).
    BonusVs(CreatureKind),
}

impl ClassPassiveStat {
    /// Fold `amount` (already scaled by skill level) into the matching `Stats`
    /// field, mirroring `ClassAttributes::apply`/racial conventions: the BL-52
    /// to-hit/resist layer is additive; the `*_modifier` / `mult_mod` fields
    /// are multiplicative (they default to 1.0 after the per-tick reset).
    pub fn apply(self, stats: &mut Stats, amount: f32) {
        match self {
            ClassPassiveStat::MaxHealth => stats.max_health_modifiers.mult_mod *= 1.0 + amount,
            ClassPassiveStat::MaxEnergy => stats.max_energy_modifiers.mult_mod *= 1.0 + amount,
            ClassPassiveStat::AttackDamage => stats.attack_damage_modifier *= 1.0 + amount,
            ClassPassiveStat::SpellPower => stats.spell_power *= 1.0 + amount,
            ClassPassiveStat::HealPower => stats.heal_power *= 1.0 + amount,
            ClassPassiveStat::Accuracy => stats.accuracy += amount,
            ClassPassiveStat::Evasion => stats.evasion += amount,
            ClassPassiveStat::MagicAccuracy => stats.magic_accuracy += amount,
            ClassPassiveStat::CritChance => stats.crit_chance += amount,
            ClassPassiveStat::PrecisionMult => stats.precision_power_mult *= 1.0 + amount,
            ClassPassiveStat::ResistFire => stats.resist_fire += amount,
            ClassPassiveStat::ResistFrost => stats.resist_frost += amount,
            ClassPassiveStat::ResistPoison => stats.resist_poison += amount,
            ClassPassiveStat::ResistMagic => stats.resist_magic += amount,
            ClassPassiveStat::CrowdControlResistance => stats.crowd_control_resistance += amount,
            ClassPassiveStat::DamageReduction => stats.damage_reduction.pos_mod += amount,
            ClassPassiveStat::MitigationsPenetration => stats.mitigations_penetration += amount,
            ClassPassiveStat::PoiseDamage => stats.poise_damage_modifier *= 1.0 + amount,
            ClassPassiveStat::MoveSpeed => stats.move_speed_modifier *= 1.0 + amount,
            ClassPassiveStat::RecoverySpeed => stats.recovery_speed_modifier *= 1.0 + amount,
            ClassPassiveStat::EnergyReward => stats.energy_reward_modifier *= 1.0 + amount,
            ClassPassiveStat::EnergyEfficiency => stats.energy_efficiency_modifier *= 1.0 + amount,
            ClassPassiveStat::EnergyRegen => stats.energy_regen_modifier *= 1.0 + amount,
            ClassPassiveStat::BonusVs(kind) => stats.bonus_damage_vs[kind as usize] += amount,
        }
    }
}
