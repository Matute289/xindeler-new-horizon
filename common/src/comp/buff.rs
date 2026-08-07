use crate::{
    combat::{
        AttackEffect, AttackSource, AttackedModification, AttackedModifier, CombatBuff,
        CombatBuffStrength, CombatEffect, CombatModification, CombatRequirement, Knockback,
        KnockbackDir, ScalingKind, StatEffect, StatEffectTarget,
    },
    comp::{
        FrontendMarker, Mass, Stats,
        ability::MagicSource,
        aura::AuraKey,
        detection::SenseKind,
        projectile::{
            ProjectileArcingProperties, ProjectileConstructorEffect,
            ProjectileConstructorEffectKind,
        },
        stats::ResistKind,
        tool::ToolKind,
    },
    link::DynWeakLinkHandle,
    match_some,
    resources::{Secs, Time},
    uid::Uid,
};

use core::cmp::Ordering;
use enum_map::{Enum, EnumMap};
use itertools::Either;
use serde::{Deserialize, Serialize};
use slotmap::{SlotMap, new_key_type};
use specs::{Component, DerefFlaggedStorage, VecStorage};
use strum::EnumIter;

use super::Body;

new_key_type! { pub struct BuffKey; }

/// De/buff Kind.
/// This is used to determine what effects a buff will have
#[derive(
    Clone, Copy, PartialEq, Eq, Hash, Debug, Serialize, Deserialize, PartialOrd, Ord, EnumIter, Enum,
)]
pub enum BuffKind {
    // =================
    //       BUFFS
    // =================
    /// Restores health/time for some period.
    /// Strength should be the healing per second.
    Regeneration,
    /// Restores health/time for some period for consumables.
    /// Strength should be the healing per second.
    Saturation,
    /// Applied when drinking a potion.
    /// Strength should be the healing per second.
    Potion,
    /// Increases movement speed and vulnerability to damage as well as
    /// decreases the amount of damage dealt.
    /// Movement speed increases linearly with strength 1.0 is an 100% increase
    /// Damage vulnerability and damage reduction are both hard set to 100%
    Agility,
    /// Applied when resting (sitting at campfire or sleeping).
    /// Strength is fraction of health restored per second.
    RestingHeal,
    /// Restores energy/time for some period.
    /// Strength should be the energy regenerated per second.
    EnergyRegen,
    /// Generates combo over time for some period.
    /// Strength should be the combo generated per second.
    ComboGeneration,
    /// Raises maximum energy.
    /// Strength should be 10x the effect to max energy.
    IncreaseMaxEnergy,
    /// Raises maximum health.
    /// Strength should be the effect to max health.
    IncreaseMaxHealth,
    /// Temp-HP / absorb shield (BL-05 RD-6). Grants an absorb pool equal to
    /// `strength` that soaks incoming damage before real HP. The pool lives on
    /// `Health.absorb` (set when this buff is added, cleared when removed); the
    /// buff has no per-tick effect and persists (no timer) until the pool is
    /// depleted, then it is removed. Same-kind re-apply refreshes
    /// (take-higher).
    Shielded,
    /// Makes you immune to attacks.
    /// Strength does not affect this buff.
    Invulnerability,
    /// Reduces incoming damage.
    /// Strength scales the damage reduction non-linearly. 0.5 provides 50% DR,
    /// 1.0 provides 67% DR.
    ProtectingWard,
    /// Increases movement speed and gives health regeneration.
    /// Strength scales the movement speed linearly. 0.5 is 150% speed, 1.0 is
    /// 200% speed. Provides regeneration at 10x the value of the strength.
    Frenzied,
    /// Increases movement and attack speed Strength scales strength of both
    /// effects linearly. 0.5 is a 50% increase, 1.0 is a 100% increase.
    Hastened,
    /// Immunity to `DifficultTerrain` (BL-03) — "freedom of movement", granted
    /// by an item / spell / class (e.g. a Ranger at home in the wilds). No
    /// other effect. Reactive immunity: it strips the re-applied debuff
    /// each tick via the existing `BuffImmunity` model (same as
    /// Frozen→Chilled).
    FreedomOfMovement,
    /// Immunity to `Terrified` -- an exact mirror of `FreedomOfMovement`'s
    /// shape: a tag-only buff with no numeric effect of its own, just a flag
    /// that strips a re-applied `Terrified` the same reactive-immunity way
    /// `FreedomOfMovement` strips `DifficultTerrain`.
    Fearless,
    /// Increases resistance to incoming poise, and poise damage dealt as health
    /// is lost.
    /// Strength scales the resistance non-linearly. 0.5 provides 50%, 1.0
    /// provides 67%.
    /// Strength scales the poise damage increase linearly, a strength of 1.0
    /// and n health less from maximum health will cause poise damage to
    /// increase by n%.
    Fortitude,
    /// Increases both attack damage and vulnerability to damage.
    /// Damage increases linearly with strength, 1.0 is a 100% increase.
    /// Damage reduction decreases linearly with strength, 1.0 is a 100%
    /// decrease.
    Reckless,
    /// Provides immunity to burning and increases movement speed in lava.
    /// Movement speed increases linearly with strength, 1.0 is a 100% increase.
    // SalamanderAspect, TODO: Readd in second dwarven mine MR
    /// Your attacks cause targets to receive the burning debuff
    /// Strength of burning debuff is a fraction of the damage, fraction
    /// increases linearly with strength
    Flame,
    /// Your attacks cause targets to receive the frozen debuff
    /// Strength of frozen debuff is equal to the strength of this buff
    Frigid,
    /// Your attacks have lifesteal
    /// Strength increases the fraction of damage restored as life
    Lifesteal,
    /// Your attacks against bleeding targets have lifesteal
    /// Strength increases the fraction of damage restored as life
    Bloodfeast,
    /// Guarantees that the next attack is a precise hit. Does this kind of
    /// hackily by adding 100% to the precision, will need to be adjusted if we
    /// ever allow double precision hits instead of treating 100 as a
    /// ceiling.
    ImminentCritical,
    /// Increases combo gain, every 1 strength increases combo per strike by 1,
    /// rounds to nearest integer
    Fury,
    /// Allows attacks to ignore DR and increases energy reward
    /// DR penetration is non-linear, 0.5 is 50% penetration and 1.0 is a 67%
    /// penetration. Energy reward is increased linearly to strength, 1.0 is a
    /// 150 % increase.
    Sunderer,
    /// Generates combo when damaged.
    /// Combo generation is linear with strength, 1.0 is 5 combo generated
    /// on being hit.
    Defiance,
    /// Increases both attack damage, vulnerability to damage, attack speed, and
    /// movement speed Damage increases linearly with strength, 1.0 is a
    /// 100% increase. Damage reduction decreases linearly with strength,
    /// 1.0 is a 100% Attack speed increases non-linearly with strength, 0.5
    /// is a 25% increase, 1.0 is a 33% increase Movement speed increases
    /// non-linearly with strength, 0.5 is a 12.5% increase, 1.0 is a 16.7%
    /// increase decrease.
    Berserk,
    /// Increases poise resistance and energy reward. However if killed, buffs
    /// killer with Reckless buff. Poise resistance scales non-linearly with
    /// strength, 0.5 is 50% and 1.0 is 67%. Energy reward scales linearly with
    /// strength, 0.5 is +50% and 1.0 is +100% strength. Reckless buff reward
    /// strength is equal to scornful taunt buff strength.
    ScornfulTaunt,
    /// Increases damage resistance, causes energy to be generated when damaged,
    /// and decreases movement speed. Damage resistance increases non-linearly
    /// with strength, 0.5 is 25% and 1.0 is 34%. Energy generation is linear
    /// with strength, 1.0 is 10 energy per hit. Movement speed is decreased to
    /// 70%.
    Tenacity,
    /// Applies to some debuffs that have strong CC effects. Automatically
    /// gained upon receiving those debuffs, and causes future instances of
    /// those debuffs to be applied with reduced duration.
    /// Strength linearly decreases the duration of newly applied, affected
    /// debuffs, 0.5 is a 50% reduction.
    Resilience,
    /// Causes successful bow weapon attacks to cause you to gain the hastened
    /// buff for a few seconds.
    /// StormChaser strength is used as strength for Hastened.
    StormChaser,
    /// Causes projectile attacks to have more precision power, and to guarantee
    /// a minimum precision multiplier.
    /// Strength linearly increases both. The minimum precision power is
    /// equivalent to the buff strength, and the additional precision power is
    /// 50% of the buff strength.
    EagleEye,
    /// Causes the next attack to have precision of 1.0 if the target is not
    /// wielding their weapon, and also generally increases damage.
    /// Strength linearly increases the damage increase.
    OwlTalon,
    /// Causes the next projectile fired to have more knockback and poise
    /// damage.
    /// Strength linearly increases the knockback and poise damage applied to
    /// the next projectile.
    HeavyNock,
    /// Causes the next projectile to both gain precision and restore more
    /// energy.
    /// Strength linearly increases the precision override and energy restored.
    Heartseeker,
    /// Causes the next projectile fired to debuff the target with ArdentHunted.
    /// Projectiles fired at the target generate additional combo, and
    /// increase energy reward by a percentage.
    /// Strength linearly increases the amount of additional combo generated and
    /// the additional energy reward.
    ArdentHunter,
    /// Causes the next projectile fired to do additional damage for every
    /// debuff the target has that had been inflicted by the attacker when using
    /// a bow.
    /// Strength linearly increases the amount of additional damage.
    SepticShot,
    /// Marks a specific entity to be affected by projectiles fired while this
    /// buff is active.
    /// Projectiles fired at this target deal additional damage, while
    /// projectiles fired at other targets deal reduced damage.
    /// Strength linearly increases the amount of additional damage dealt to the
    /// target. Damage reduction against other targets is fixed at -25%.
    ArdentHunt,
    /// Causes the next projectile fired by a bow to add the burning debuff to
    /// the target.
    /// Strength linearly increases the fraction of damage converted to burning
    /// (0.5 -> +50%, 1.0 -> +100%).
    IgniteArrow,
    /// Causes the next projectile fired by a bow to add the frozen debuff to
    /// the target.
    /// The strength of this buff becomes the strength of the frozen debuff.
    FreezeArrow,
    /// Causes the next projectile fired by a bow to add the poisoned debuff to
    /// the target.
    /// The strength of this buff becomes the strength of the poisoned debuff.
    DrenchArrow,
    /// Causes the next projectile fired by a bow to change to an arcing
    /// projectile.
    /// The strength of this buff is multiplied by the damage on the original
    /// projectile to determine the arcing damage.
    JoltArrow,
    /// Causes the wielder's next weapon hit to deal additional damage and
    /// blind the target. Strength scales the additional damage; the target's
    /// `Blinded` duration is fixed.
    BlindingSmite,
    /// Causes the wielder's next weapon hit to deal additional damage, no
    /// secondary effect. Strength scales the additional damage.
    BrandingSmite,
    /// Causes the wielder's next weapon hit to deal additional damage, no
    /// secondary effect — a larger bonus than `BrandingSmite`. Strength
    /// scales the additional damage.
    DivineSmite,
    /// Causes the wielder's next weapon hit to deal additional damage and set
    /// the target burning. Strength scales the additional damage; the
    /// target's `Burning` duration/strength are fixed.
    SearingSmite,
    /// Causes the wielder's next weapon hit to deal additional damage and
    /// poise damage. Strength scales the additional damage; the poise
    /// damage is fixed.
    StaggeringSmite,
    /// Causes the wielder's next weapon hit to deal additional damage and
    /// knock the target back. Strength scales the additional damage; the
    /// knockback strength is fixed.
    ThunderousSmite,
    /// Causes the wielder's next weapon hit to deal additional damage and
    /// frighten the target. Strength scales the additional damage; the
    /// target's `Terrified` duration is fixed.
    WrathfulSmite,
    /// Sharpens the bearer's attacks and resolve. Strength scales the flat
    /// accuracy bonus.
    Blessed,
    /// Grants the bearer's weapon attacks additional damage for as long as
    /// the buff holds — unlike the smites above, not consumed after one hit
    /// (granted by an aura, not a self-buff with `RemoveOnAttack`). Strength
    /// scales the additional damage.
    CrusadersMantle,
    /// A voluntary, restful sleep granted to a willing target — mechanically
    /// identical to `Asleep` (same incapacitation, same wake-on-damage rule)
    /// but classified as a positive buff rather than a debuff, since it is
    /// cast on consenting allies rather than inflicted on foes.
    RestfulSleep,
    /// A defensive ward against otherworldly creatures. Mechanically a flat
    /// `DamageReduction` identical to `ProtectingWard` (there is no
    /// creature-kind-conditional resistance primitive to gate it on the
    /// attacker's `CreatureKind`, so v1 reduces all incoming damage while
    /// the ward holds rather than only damage from
    /// Celestial/Elemental/Fey/Fiend/Undead attackers). Strength scales the
    /// damage reduction non-linearly, same curve as `ProtectingWard`.
    OtherworldlyWard,
    // =================
    //      DEBUFFS
    // =================
    /// Does damage to a creature over time.
    /// Strength should be the DPS of the debuff.
    /// Provides immunity against Frozen.
    Burning,
    /// Lowers health over time for some duration.
    /// Strength should be the DPS of the debuff.
    /// Frays the bearer's focus. Strength scales the flat accuracy penalty
    /// (the mirror of `Blessed`).
    Bane,
    /// Marks the bearer with a glow that makes them easier to hit. Strength
    /// scales the flat evasion penalty (the mirror of `Blur`'s positive
    /// evasion).
    FaerieFire,
    /// Withers the bearer's physical strikes. Strength scales the fraction
    /// of outgoing attack damage retained (0.5 -> half damage), same shape
    /// as `Blinded`.
    Enfeebled,
    Bleeding,
    /// Bleed-detonate (BL-05 RD-7): bleeds over time like `Bleeding`, then
    /// **detonates** for a burst of damage when it runs its full course
    /// (natural expiry). Dispelling/cleansing it early prevents the blast.
    /// Strength is the bleed DPS; the detonation is a multiple of it
    /// (`BLEED_DETONATE_MULT`).
    BleedingMark,
    /// Lower a creature's max health over time.
    /// Strength only affects the target max health, 0.5 targets 50% of base
    /// max, 1.0 targets 100% of base max.
    Cursed,
    /// Reduces movement speed and causes bleeding damage.
    /// Strength scales the movement speed debuff non-linearly. 0.5 is 50%
    /// speed, 1.0 is 33% speed. Bleeding is at 4x the value of the strength.
    Crippled,
    /// Slows movement and attack speed and increases poise damage received.
    /// Strength scales the attack speed debuff non-linearly. 0.5 is ~50%
    /// speed, 1.0 is 33% speed. Movement speed debuff is scaled to be slightly
    /// smaller than attack speed debuff. Received poise damage scales linearly,
    /// 1.0 is a 100% increase.
    /// Provides immunity against Heatstroke and Chilled.
    Frozen,
    /// Makes you wet and causes you to have reduced friction on the ground.
    /// Strength scales the friction you ignore non-linearly. 0.5 is 50% ground
    /// friction, 1.0 is 33% ground friction.
    /// Provides immunity against Burning.
    Wet,
    /// Makes you move slower.
    /// Strength scales the movement speed debuff non-linearly. 0.5 is 50%
    /// speed, 1.0 is 33% speed.
    Ensnared,
    /// Drain stamina to a creature over time.
    /// Strength should be the energy per second of the debuff.
    Poisoned,
    /// Results from having an attack parried.
    /// Causes your attack speed to be slower to emulate the recover duration of
    /// an ability being lengthened.
    Parried,
    /// Results from drinking a potion.
    /// Decreases the health gained from subsequent potions.
    PotionSickness,
    /// Slows movement speed and reduces energy reward.
    /// Both scales non-linearly to strength, 0.5 lead to movespeed reduction
    /// by 25% and energy reward reduced by 150%, 1.0 lead to MS reduction by
    /// 33.3% and energy reward reduced by 200%. Energy reward can't be
    /// reduced by more than 200%, to a minimum value of -100%.
    Heatstroke,
    /// Reduces movement speed to 0.
    /// Strength increases the relative mass of the creature that can be
    /// targeted. A strength of 1.0 means that a creature of the same mass gets
    /// rooted for the full duration. A strength of 2.0 means a creature of
    /// twice the mass gets rooted for the full duration. If the target's mass
    /// is higher than the strength allows for, duration gets reduced using a
    /// mutiplier from the ratio of masses.
    Rooted,
    /// Slows movement speed and reduces energy reward
    /// Both scale non-linearly with strength, 0.5 leads to 50% reduction of
    /// energy reward and 33% reduction of move speed. 1.0 leads to 67%
    /// reduction of energy reward and 50% reduction of move speed.
    Winded,
    /// Prevents use of auxiliary abilities.
    /// Does not scale with strength
    Amnesia,
    /// Increases amount of poise damage received
    /// Scales linearly with strength, 1.0 leads to 100% more poise damage
    /// received
    OffBalance,
    /// Decreases movement speed and increases amount of poise damage received.
    /// Movement speed decreases non-linearly with strength, 0.5 leads to a 25%
    /// reduction, 1.0 leads to a 33% reduction. Poise damage received scales
    /// linearly with strength, 1.0 leads to 100% more poise damage.
    /// Provides immunity to Heatstroke.
    Chilled,
    /// Increases combo generation and energy reward when hit with projectiles.
    /// Strength linearly increases the amount of additional combo generated and
    /// the additional energy reward.
    ArdentHunted,
    /// Dread of death. Heavy movement slow (players); NPCs additionally rout
    /// (server/agent). Strength scales the slow non-linearly like Crippled.
    /// v1 implements the spec's sanctioned fallback (slow, not forced
    /// movement) to avoid prediction artifacts; see magic spec §5 risk note.
    Terrified,
    /// Cannot bring itself to harm the charmer. No stat effects; consumed by
    /// agent targeting (NPCs only in v1, spec §5).
    Charmed,
    /// Obeys the dominator's commands. No stat effects — a state tag, same
    /// shape as `Charmed`; consumed by the shared command-authorisation
    /// predicate (NPCs only, players can never be commanded).
    Dominated,
    /// Turned hostile against everyone, including former allies. No stat
    /// effects — a state tag, same shape as `Charmed`; consumed by agent
    /// targeting.
    Maddened,
    /// Cannot move, use auxiliary abilities, or deal damage — a harder lock
    /// than `Asleep`, and deliberately does NOT wake on taking damage (see
    /// `Asleep`'s own wake-on-hit rule, which this must not inherit). Always
    /// paired with `resilience_ccr_strength` for automatic diminishing
    /// returns on repeated application.
    Paralyzed,
    /// The Hollow's surcharge: stacking multiplicative max-health reduction
    /// applied by every Beyond-tainted Necromancy cast via
    /// AbilityMeta.init_event.
    Hollowtouched,
    /// Difficult terrain (BL-03): movement slowed while inside the zone —
    /// `MovementSpeed(1.0 - strength)`, so strength 0.5 = half speed. Delivered
    /// by an aura (spell / terrain / weather); negated by `FreedomOfMovement`.
    DifficultTerrain,
    /// Antimagic field (BL-36): inside the zone, magic abilities can't be cast
    /// and attuned magic-item effects are suppressed (physical/innate abilities
    /// unaffected). Indiscriminate — applied to all in the zone.
    /// `DisableMagic`.
    Antimagic,
    /// Dimensional anchor (BL-05 rider): the bearer can't teleport/blink while
    /// it lasts (e.g. "Immovable Object" / anti-teleport zones). Sets
    /// `DisableTeleport`. Other movement is unaffected.
    Anchored,
    /// Magical sleep (BL-05 rider): the bearer is incapacitated — can't move
    /// (`MovementSpeed(0)`) and can't use auxiliary abilities
    /// (`DisableAuxiliaryAbilities`). v1 is duration-based; wake-on-damage is a
    /// documented follow-up (spell-riders-engine spec §6 / tasks/13).
    Asleep,
    /// Blinded (BL-05 rider): can't aim — outgoing attack damage is reduced by
    /// `strength` (`AttackDamage(1 - strength)`), e.g. strength 0.5 = half
    /// damage dealt. The action-combat analogue of "attack disadvantage";
    /// vision occlusion is a deferred client-only effect.
    Blinded,
    /// Generic movement slow (BL-66 d), for Slow spells and other sources
    /// that should reduce movement speed without any other side effect.
    /// Mirrors `Crippled`'s movement-speed curve but WITHOUT the HP drain.
    /// Strength scales the movement speed debuff non-linearly, 0.5 is 50%
    /// speed, 1.0 is 33% speed.
    Slowed,
    /// Incapacitation combining halved movement (`Slowed`'s curve), reduced
    /// outgoing attack damage (`Blinded`'s curve, the disadvantage
    /// analogue), and disabled auxiliary abilities (`Amnesia`'s effect) all
    /// at once. Unlike `Asleep`/`Paralyzed`, movement and damage are reduced
    /// rather than zeroed — the target is debilitated, not incapacitated
    /// outright. The ability lock does not scale with strength.
    Agonized,
    /// Folds `Bane`'s flat accuracy penalty and a `Bleeding`-style health
    /// drain tick into one buff, so a target hit by both effects at once
    /// shows a single icon/tooltip rather than two. Strength scales both:
    /// the accuracy penalty follows `Bane`'s own `nn_scaling` curve, and the
    /// drain rate is `strength` HP/tick, the same units `Bleeding` uses.
    Sickened,
    /// Reduces the bearer's own critical-hit chance -- the third hexable
    /// aspect alongside `Bane` (accuracy) and `FaerieFire` (evasion).
    /// Strength scales the flat reduction non-linearly, same curve as those
    /// two.
    Hexed,
    // =================
    //      COMPLEX
    // =================
    /// Changed into another body.
    Polymorphed,
    // =================
    //    DETECTION
    // =================
    /// A generic active magical sense (e.g. Detect Magic and its kin):
    /// area-of-effect, one-shot or timed, membership frozen at cast.
    Detecting,
    /// Lets the bearer perceive concealed/invisible creatures. Sets a
    /// perception-piercing flag; produces no reveal set of its own.
    SeeInvisible,
    /// An always-active, continuously re-evaluated true sense: pierces
    /// concealment, illusion and darkness for as long as it lasts, and
    /// reveals concealed/illusory entities as they come into range.
    TrueSight,
    // =================
    //   REMOTE SENSE
    // =================
    /// Declares an active remote-sensing link: the bearer's point of view is
    /// anchored somewhere other than its own body (see `RemoteSense` /
    /// `SenseAnchor`). Always tagged `BuffCategory::Concentration` (see
    /// `extend_cat_ids`) — a solid hit ends the link the same way it ends any
    /// other concentration effect, and only one such link is held at a time.
    RemoteSensing,
    // =================
    //     DISGUISE
    // =================
    /// A cast disguise: cosmetic-only, unlike `Polymorphed` above. Declares
    /// the shape of the bearer's `Disguise` component (see its own doc
    /// comment) without touching `Body`, `Mass`, `Density`, `Collider` or any
    /// combat stat. Authors should tag the granting buff/ability
    /// `BuffCategory::RemoveOnAttack` — a disguise breaking the instant its
    /// wearer attacks is a content/RON decision, not something this variant
    /// enforces in code.
    Disguised,
    // =================
    //    CONCEALMENT
    // =================
    /// A hush-of-shadow ward granted to allies caught in the caster's aura:
    /// contributes `data.strength` to `Stats.stealth`, the same additive
    /// channel item-based stealth feeds (see
    /// `combat::perception_dist_multiplier_from_stealth`). No other effect.
    /// Not tagged `BuffCategory::Concentration`: this buff lives on the
    /// allies the aura reaches, not on the caster, and its lifetime is
    /// already fully governed by the granting `BasicAura`'s own
    /// `aura_duration` (the `FromActiveAura` category the aura system adds
    /// removes the buff the instant a target leaves radius or the aura
    /// itself expires) -- there is no separate concentration-break-on-hit
    /// concept for aura-sourced buffs to opt into today.
    PassWithoutTrace,
    /// A moonlit concealment ward granted to allies caught in the caster's
    /// aura: contributes `data.strength` to `Stats.stealth` exactly like
    /// `PassWithoutTrace`, plus a smaller derived `ResistKind::Magic`
    /// resistance (see `effects()` for the fixed ratio between the two).
    /// Same `BuffCategory::Concentration` reasoning as `PassWithoutTrace`
    /// above applies here too.
    Mooncloak,
    // =================
    //  ANTI-DIVINATION
    // =================
    /// A screening ward woven over the buffed entity so scrying and
    /// far-seeing magic slide harmlessly past it. Sets `Stats.nondetection`
    /// unconditionally, no other effect. Self-cast: this is a `SelfBuff`
    /// (see `nondetection.ron`), not an aura, so unlike `PassWithoutTrace`/
    /// `Mooncloak` above it has no target-selection story of its own.
    Nondetection,
    /// A false reading fed to whoever probes the buffed entity with
    /// detection magic: it registers as matching the authored `SenseKind`
    /// regardless of its real nature. The `SenseKind` to lie about is
    /// authored via `misc_data` (see `MiscBuffData::FalseAura`), the same
    /// "authored rider, not derived from `strength`" idiom `Mooncloak`'s
    /// `ResistMagic` rider above uses, since which sense to fake can't be
    /// derived from a single strength dial.
    MagicAura,
    /// Combines both of this game's anti-perception primitives on one
    /// target: unconditional anti-divination (`Nondetection`, see above)
    /// plus a very large `Stealth` contribution meant to drive
    /// `combat::perception_dist_multiplier_from_stealth` toward zero --
    /// "functionally invisible", not a render-hide. See `sequester.ron`'s
    /// own comment for the chosen strength and the resulting multiplier.
    Sequester,
}

/// Tells a little more about the buff kind than simple buff/debuff
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BuffDescriptor {
    /// Simple positive buffs, like `BuffKind::Saturation`
    SimplePositive,
    /// Simple negative buffs, like `BuffKind::Bleeding`
    SimpleNegative,
    /// Buffs that require unusual data that can't be governed just by strength
    /// and duration, like `BuffKind::Polymorhped`
    Complex,
    // For future additions, we may want to tell about non-obvious buffs,
    // like Agility.
    // Also maybe extend Complex to differentiate between Positive, Negative
    // and Neutral buffs?
    // For now, Complex is assumed to be neutral/non-obvious.
}

impl BuffKind {
    /// Tells a little more about buff kind than simple buff/debuff
    ///
    /// Read more in [BuffDescriptor].
    pub fn differentiate(self) -> BuffDescriptor {
        match self {
            BuffKind::Regeneration
            | BuffKind::Saturation
            | BuffKind::Potion
            | BuffKind::Agility
            | BuffKind::RestingHeal
            | BuffKind::Frenzied
            | BuffKind::EnergyRegen
            | BuffKind::ComboGeneration
            | BuffKind::IncreaseMaxEnergy
            | BuffKind::IncreaseMaxHealth
            | BuffKind::Shielded
            | BuffKind::Invulnerability
            | BuffKind::ProtectingWard
            | BuffKind::Hastened
            | BuffKind::Fortitude
            | BuffKind::Reckless
            | BuffKind::Flame
            | BuffKind::Frigid
            | BuffKind::Lifesteal
            //| BuffKind::SalamanderAspect
            | BuffKind::ImminentCritical
            | BuffKind::Fury
            | BuffKind::Sunderer
            | BuffKind::Defiance
            | BuffKind::Bloodfeast
            | BuffKind::Berserk
            | BuffKind::ScornfulTaunt
            | BuffKind::Tenacity
            | BuffKind::Resilience
            | BuffKind::StormChaser
            | BuffKind::EagleEye
            | BuffKind::OwlTalon
            | BuffKind::HeavyNock
            | BuffKind::Heartseeker
            | BuffKind::ArdentHunter
            | BuffKind::SepticShot
            | BuffKind::ArdentHunt
            | BuffKind::IgniteArrow
            | BuffKind::FreezeArrow
            | BuffKind::DrenchArrow
            | BuffKind::JoltArrow
            | BuffKind::BlindingSmite
            | BuffKind::BrandingSmite
            | BuffKind::DivineSmite
            | BuffKind::SearingSmite
            | BuffKind::StaggeringSmite
            | BuffKind::ThunderousSmite
            | BuffKind::WrathfulSmite
            | BuffKind::Blessed
            | BuffKind::CrusadersMantle
            | BuffKind::RestfulSleep
            | BuffKind::OtherworldlyWard
            | BuffKind::FreedomOfMovement
            | BuffKind::Fearless
            | BuffKind::Detecting
            | BuffKind::SeeInvisible
            | BuffKind::TrueSight
            | BuffKind::RemoteSensing
            | BuffKind::PassWithoutTrace
            | BuffKind::Mooncloak
            | BuffKind::Nondetection
            | BuffKind::MagicAura
            | BuffKind::Sequester => BuffDescriptor::SimplePositive,
            BuffKind::Bleeding
            | BuffKind::BleedingMark
            | BuffKind::Bane
            | BuffKind::FaerieFire
            | BuffKind::Enfeebled
            | BuffKind::Cursed
            | BuffKind::Burning
            | BuffKind::Crippled
            | BuffKind::Frozen
            | BuffKind::Wet
            | BuffKind::Ensnared
            | BuffKind::Poisoned
            | BuffKind::Parried
            | BuffKind::PotionSickness
            | BuffKind::Heatstroke
            | BuffKind::Rooted
            | BuffKind::Winded
            | BuffKind::Amnesia
            | BuffKind::OffBalance
            | BuffKind::Chilled
            | BuffKind::ArdentHunted
            | BuffKind::Terrified
            | BuffKind::Charmed
            | BuffKind::Dominated
            | BuffKind::Maddened
            | BuffKind::Paralyzed
            | BuffKind::Hollowtouched
            | BuffKind::DifficultTerrain
            | BuffKind::Antimagic
            | BuffKind::Anchored
            | BuffKind::Asleep
            | BuffKind::Blinded
            | BuffKind::Slowed
            | BuffKind::Agonized
            | BuffKind::Sickened
            | BuffKind::Hexed => BuffDescriptor::SimpleNegative,
            BuffKind::Polymorphed | BuffKind::Disguised => BuffDescriptor::Complex,
        }
    }

    /// Checks if buff is buff or debuff.
    pub fn is_buff(self) -> bool {
        match self.differentiate() {
            BuffDescriptor::SimplePositive => true,
            BuffDescriptor::SimpleNegative | BuffDescriptor::Complex => false,
        }
    }

    /// Healing kinds whose strength should scale with the caster's
    /// `heal_power`.
    pub fn is_heal(self) -> bool {
        matches!(
            self,
            BuffKind::Regeneration | BuffKind::Saturation | BuffKind::RestingHeal
        )
    }

    pub fn is_simple(self) -> bool {
        match self.differentiate() {
            BuffDescriptor::SimplePositive | BuffDescriptor::SimpleNegative => true,
            BuffDescriptor::Complex => false,
        }
    }

    /// Checks if buff should queue.
    pub fn queues(self) -> bool { matches!(self, BuffKind::Saturation) }

    /// Checks if the buff can affect other buff effects applied in the same
    /// tick.
    pub fn affects_subsequent_buffs(self) -> bool {
        matches!(
            self,
            BuffKind::PotionSickness /* | BuffKind::SalamanderAspect */
        )
    }

    /// Checks if multiple instances of the buff should be processed, instead of
    /// only the strongest.
    pub fn stacks(self) -> bool {
        matches!(
            self,
            BuffKind::PotionSickness | BuffKind::Resilience | BuffKind::Hollowtouched
        )
    }

    pub fn effects(
        &self,
        data: &BuffData,
        target_entity: Option<Uid>,
        source_entity: Option<Uid>,
    ) -> Vec<BuffEffect> {
        // Normalized nonlinear scaling
        // TODO: Do we want to make denominator term parameterized. Come back to if we
        // add nn_scaling3.
        let nn_scaling = |a: f32| a.abs() / (a.abs() + 0.5) * a.signum();
        let nn_scaling_custom = |a: f32, b: f32| a.abs() / (a.abs() + b.max(0.1)) * a.signum();
        let instance = rand::random();
        match self {
            BuffKind::Bleeding => vec![BuffEffect::HealthChangeOverTime {
                rate: -data.strength,
                kind: ModifierKind::Additive,
                instance,
                tick_dur: Secs(0.5),
            }],
            // BL-05 RD-7: bleeds like `Bleeding` while active; the on-expire
            // detonation burst is emitted by the buff system (the `effects()`
            // here are only the per-tick bleed).
            BuffKind::BleedingMark => vec![BuffEffect::HealthChangeOverTime {
                rate: -data.strength,
                kind: ModifierKind::Additive,
                instance,
                tick_dur: Secs(0.5),
            }],
            BuffKind::Regeneration => vec![BuffEffect::HealthChangeOverTime {
                rate: data.strength,
                kind: ModifierKind::Additive,
                instance,
                tick_dur: Secs(1.0),
            }],
            BuffKind::Saturation => vec![BuffEffect::HealthChangeOverTime {
                rate: data.strength,
                kind: ModifierKind::Additive,
                instance,
                tick_dur: Secs(3.0),
            }],
            BuffKind::Potion => {
                vec![BuffEffect::HealthChangeOverTime {
                    rate: data.strength,
                    kind: ModifierKind::Additive,
                    instance,
                    tick_dur: Secs(0.1),
                }]
            },
            BuffKind::Agility => vec![
                BuffEffect::MovementSpeed(1.0 + data.strength),
                BuffEffect::DamageReduction(-1.0),
                BuffEffect::AttackDamage(0.0),
            ],
            BuffKind::RestingHeal => vec![BuffEffect::HealthChangeOverTime {
                rate: data.strength,
                kind: ModifierKind::Multiplicative,
                instance,
                tick_dur: Secs(2.0),
            }],
            BuffKind::Cursed => vec![
                BuffEffect::MaxHealthChangeOverTime {
                    rate: -1.0,
                    kind: ModifierKind::Additive,
                    target_fraction: 1.0 - data.strength,
                },
                BuffEffect::HealthChangeOverTime {
                    rate: -1.0,
                    kind: ModifierKind::Additive,
                    instance,
                    tick_dur: Secs(0.5),
                },
            ],
            BuffKind::EnergyRegen => vec![BuffEffect::EnergyChangeOverTime {
                rate: data.strength,
                kind: ModifierKind::Additive,
                tick_dur: Secs(0.25),
                reset_rate_on_tick: false,
            }],
            BuffKind::ComboGeneration => {
                let target_tick_dur = 0.25;
                // Combo per tick must be an integer
                let nearest_valid_tick_dur =
                    (data.strength as f64 * target_tick_dur).round() / data.strength as f64;

                vec![BuffEffect::ComboChangeOverTime {
                    rate: data.strength,
                    tick_dur: Secs(nearest_valid_tick_dur),
                }]
            },
            BuffKind::IncreaseMaxEnergy => vec![BuffEffect::MaxEnergyModifier {
                value: data.strength,
                kind: ModifierKind::Additive,
            }],
            BuffKind::IncreaseMaxHealth => vec![BuffEffect::MaxHealthModifier {
                value: data.strength,
                kind: ModifierKind::Additive,
            }],
            // BL-05 RD-6: no per-tick stat effect — the absorb pool is granted on
            // buff-add (`Health.raise_absorb_to`) and depleted in `change_by`.
            BuffKind::Shielded => Vec::new(),
            BuffKind::Invulnerability => vec![BuffEffect::DamageReduction(1.0)],
            BuffKind::ProtectingWard => vec![BuffEffect::DamageReduction(
                // Causes non-linearity in effect strength, but necessary
                // to allow for tool power and other things to affect the
                // strength. 0.5 also still provides 50% damage reduction.
                nn_scaling(data.strength),
            )],
            BuffKind::OtherworldlyWard => {
                vec![BuffEffect::DamageReduction(nn_scaling(data.strength))]
            },
            BuffKind::Burning => vec![
                BuffEffect::HealthChangeOverTime {
                    rate: -data.strength,
                    kind: ModifierKind::Additive,
                    instance,
                    tick_dur: Secs(0.25),
                },
                BuffEffect::BuffImmunity(BuffKind::Frozen),
            ],
            BuffKind::Poisoned => vec![BuffEffect::EnergyChangeOverTime {
                rate: -data.strength,
                kind: ModifierKind::Additive,
                tick_dur: Secs(0.5),
                reset_rate_on_tick: true,
            }],
            BuffKind::Crippled => vec![
                BuffEffect::MovementSpeed(1.0 - nn_scaling(data.strength)),
                BuffEffect::HealthChangeOverTime {
                    rate: -data.strength * 4.0,
                    kind: ModifierKind::Additive,
                    instance,
                    tick_dur: Secs(0.5),
                },
            ],
            BuffKind::Frenzied => vec![
                BuffEffect::MovementSpeed(1.0 + data.strength),
                BuffEffect::HealthChangeOverTime {
                    rate: data.strength * 10.0,
                    kind: ModifierKind::Additive,
                    instance,
                    tick_dur: Secs(1.0),
                },
            ],
            BuffKind::Frozen => vec![
                BuffEffect::MovementSpeed(f32::powf(1.0 - nn_scaling(data.strength), 1.1)),
                BuffEffect::AttackSpeed(1.0 - nn_scaling(data.strength)),
                BuffEffect::PoiseReduction(-data.strength),
                BuffEffect::BuffImmunity(BuffKind::Heatstroke),
                BuffEffect::BuffImmunity(BuffKind::Chilled),
            ],
            BuffKind::Chilled => vec![
                BuffEffect::MovementSpeed(1.0 - 0.5 * nn_scaling(data.strength)),
                BuffEffect::PoiseReduction(-data.strength),
                BuffEffect::BuffImmunity(BuffKind::Heatstroke),
            ],
            BuffKind::Wet => vec![
                BuffEffect::GroundFriction(1.0 - nn_scaling(data.strength)),
                BuffEffect::BuffImmunity(BuffKind::Burning),
            ],
            BuffKind::ArdentHunted => {
                let projectile_req = CombatRequirement::AttackSource(AttackSource::Projectile);
                let mut energy_reward_effect =
                    AttackedModification::new(AttackedModifier::EnergyReward(data.strength))
                        .with_requirement(projectile_req.clone());
                let mut damage_mult_effect =
                    AttackedModification::new(AttackedModifier::DamageMultiplier(data.strength))
                        .with_requirement(projectile_req);
                if let Some(uid) = source_entity {
                    let attacker_req = CombatRequirement::Attacker(uid);
                    energy_reward_effect =
                        energy_reward_effect.with_requirement(attacker_req.clone());
                    damage_mult_effect = damage_mult_effect.with_requirement(attacker_req);
                }
                vec![
                    BuffEffect::AttackedModification(energy_reward_effect),
                    BuffEffect::AttackedModification(damage_mult_effect),
                ]
            },
            BuffKind::Ensnared => vec![BuffEffect::MovementSpeed(1.0 - nn_scaling(data.strength))],
            // BL-03: linear slow so strength 0.5 = exactly half speed (tunable per
            // zone). Intended strength range is [0, 1); strength >= 1.0 floors at a
            // full root (clamped), so authors who want a root should use Rooted/Ensnared.
            BuffKind::DifficultTerrain => {
                vec![BuffEffect::MovementSpeed((1.0 - data.strength).max(0.0))]
            },
            // "freedom of movement" — only negates difficult terrain.
            BuffKind::FreedomOfMovement => {
                vec![BuffEffect::BuffImmunity(BuffKind::DifficultTerrain)]
            },
            // Fear immunity — only negates Terrified.
            BuffKind::Fearless => vec![BuffEffect::BuffImmunity(BuffKind::Terrified)],
            // BL-36: antimagic — suppress magic casting + attuned magic-item effects.
            BuffKind::Antimagic => vec![BuffEffect::DisableMagic],
            // BL-05 rider: dimensional anchor — block teleport/blink only.
            BuffKind::Anchored => vec![BuffEffect::DisableTeleport],
            // BL-05 rider: magical sleep — incapacitate. No movement, no
            // auxiliary abilities, and zero outgoing attack damage (the engine
            // has no "disable all abilities" primitive, so AttackDamage(0.0)
            // neuters the still-usable primary/secondary while asleep). v1 is
            // duration-based; wake-on-damage is RD-3 (tasks/13).
            // Same incapacitation as Asleep (including the wake-on-damage
            // rule in `entity_manipulation.rs`) — see `RestfulSleep`'s own
            // doc comment for why this is a separate BuffKind rather than a
            // reuse of `Asleep`.
            BuffKind::Asleep | BuffKind::RestfulSleep => vec![
                BuffEffect::MovementSpeed(0.0),
                BuffEffect::DisableAuxiliaryAbilities,
                BuffEffect::AttackDamage(0.0),
            ],
            // Harder lock than Asleep: same incapacitation, but deliberately
            // no wake-on-damage removal (contrast Asleep's own rule, which
            // lives in the health-change handler, not here).
            BuffKind::Paralyzed => vec![
                BuffEffect::MovementSpeed(0.0),
                BuffEffect::DisableAuxiliaryAbilities,
                BuffEffect::AttackDamage(0.0),
            ],
            // BL-05 rider: blinded — reduced outgoing attack damage (can't aim).
            BuffKind::Blinded => vec![BuffEffect::AttackDamage((1.0 - data.strength).max(0.0))],
            BuffKind::Enfeebled => {
                vec![BuffEffect::AttackDamage((1.0 - data.strength).max(0.0))]
            },
            BuffKind::Bane => vec![BuffEffect::Accuracy(-12.0 * nn_scaling(data.strength))],
            BuffKind::FaerieFire => vec![BuffEffect::Evasion(-12.0 * nn_scaling(data.strength))],
            // Bane's own accuracy curve plus a Bleeding-shaped drain tick,
            // folded into one buff for a single icon/tooltip.
            BuffKind::Sickened => vec![
                BuffEffect::Accuracy(-12.0 * nn_scaling(data.strength)),
                BuffEffect::HealthChangeOverTime {
                    rate: -data.strength,
                    kind: ModifierKind::Additive,
                    instance,
                    tick_dur: Secs(0.5),
                },
            ],
            // The crit-chance analogue of Bane (accuracy) / FaerieFire
            // (evasion). Kept to a smaller magnitude than those two since
            // `Stats.crit_chance` is a raw 0..1 probability rather than an
            // accuracy-point term scaled through `hit_k`.
            // -0.055 base: every other shipped `CritChance` producer is a
            // skill/feat node worth +0.02-0.03 (`class_skill_modifiers.ron`,
            // `feat_modifiers.ron`); at hex's own strength (0.7) this lands
            // around -3.2pp, roughly one skill-tree node's worth per cast --
            // meaningful without floor-clamping every low-crit target it
            // touches, unlike the originally-authored -0.15 (~-8.75pp,
            // 3-4x a full crit-tree investment; game-balance-designer
            // finding, 2026-08-07).
            BuffKind::Hexed => vec![BuffEffect::CritChance(-0.055 * nn_scaling(data.strength))],
            // BL-66 d: generic movement slow, mirrors Crippled's speed curve
            // without the HP drain.
            BuffKind::Slowed => vec![BuffEffect::MovementSpeed(1.0 - nn_scaling(data.strength))],
            BuffKind::Agonized => vec![
                BuffEffect::MovementSpeed(1.0 - nn_scaling(data.strength)),
                BuffEffect::AttackDamage((1.0 - data.strength).max(0.0)),
                BuffEffect::DisableAuxiliaryAbilities,
            ],
            BuffKind::Hastened => vec![
                BuffEffect::MovementSpeed(1.0 + data.strength),
                BuffEffect::AttackSpeed(1.0 + data.strength),
            ],
            BuffKind::Fortitude => vec![
                BuffEffect::PoiseReduction(nn_scaling(data.strength)),
                BuffEffect::PoiseDamageFromLostHealth(data.strength),
            ],
            BuffKind::Parried => vec![BuffEffect::PrecisionVulnerabilityOverride(0.75)],
            BuffKind::PotionSickness => vec![BuffEffect::ItemEffectReduction(data.strength)],
            BuffKind::Reckless => vec![
                BuffEffect::DamageReduction(-data.strength),
                BuffEffect::AttackDamage(1.0 + data.strength),
            ],
            BuffKind::Polymorphed => {
                let mut effects = Vec::new();
                if let Some(MiscBuffData::Body(body)) = data.misc_data {
                    effects.push(BuffEffect::BodyChange(body));
                }
                effects
            },
            BuffKind::Flame => vec![BuffEffect::AttackEffect(AttackEffect::new(
                None,
                CombatEffect::Buff(CombatBuff {
                    kind: BuffKind::Burning,
                    dur_secs: data.secondary_duration.unwrap_or(Secs(5.0)),
                    strength: CombatBuffStrength::DamageFraction(data.strength),
                    chance: 1.0,
                }),
            ))],
            BuffKind::Frigid => vec![BuffEffect::AttackEffect(AttackEffect::new(
                None,
                CombatEffect::Buff(CombatBuff {
                    kind: BuffKind::Frozen,
                    dur_secs: data.secondary_duration.unwrap_or(Secs(5.0)),
                    strength: CombatBuffStrength::Value(data.strength),
                    chance: 1.0,
                }),
            ))],
            BuffKind::Lifesteal => vec![BuffEffect::AttackEffect(AttackEffect::new(
                None,
                CombatEffect::Lifesteal(data.strength),
            ))],
            /*BuffKind::SalamanderAspect => vec![
                BuffEffect::BuffImmunity(BuffKind::Burning),
                BuffEffect::SwimSpeed(1.0 + data.strength),
            ],*/
            BuffKind::Bloodfeast => vec![BuffEffect::AttackEffect(
                AttackEffect::new(None, CombatEffect::Lifesteal(data.strength))
                    .with_requirement(CombatRequirement::TargetHasBuff(BuffKind::Bleeding)),
            )],
            BuffKind::ImminentCritical => vec![BuffEffect::PrecisionModifier(None, 1.0, false)],
            BuffKind::Fury => vec![BuffEffect::AttackEffect(
                AttackEffect::new(None, CombatEffect::Combo(data.strength.round() as i32))
                    .with_requirement(CombatRequirement::AnyDamage),
            )],
            BuffKind::Sunderer => vec![
                BuffEffect::MitigationsPenetration(nn_scaling(data.strength)),
                BuffEffect::EnergyReward(1.0 + 1.5 * data.strength),
            ],
            BuffKind::Defiance => vec![BuffEffect::DamagedEffect(StatEffect::new(
                StatEffectTarget::Target,
                CombatEffect::Combo((data.strength * 5.0).round() as i32),
            ))],
            BuffKind::Berserk => vec![
                BuffEffect::DamageReduction(-data.strength),
                BuffEffect::AttackDamage(1.0 + data.strength),
                BuffEffect::AttackSpeed(1.0 + nn_scaling(data.strength) / 2.0),
                BuffEffect::MovementSpeed(1.0 + nn_scaling(data.strength) / 4.0),
            ],
            BuffKind::Heatstroke => vec![
                BuffEffect::MovementSpeed(1.0 - nn_scaling(data.strength) * 0.5),
                BuffEffect::EnergyReward((1.0 - nn_scaling(data.strength) * 3.0).max(-1.0)),
            ],
            BuffKind::ScornfulTaunt => vec![
                BuffEffect::PoiseReduction(nn_scaling(data.strength)),
                BuffEffect::EnergyReward(1.0 + data.strength),
                BuffEffect::DeathEffect(StatEffect::new(
                    StatEffectTarget::Attacker,
                    CombatEffect::Buff(CombatBuff {
                        kind: BuffKind::Reckless,
                        dur_secs: data.duration.unwrap_or(Secs(10.0)),
                        strength: CombatBuffStrength::Value(data.strength),
                        chance: 1.0,
                    }),
                )),
            ],
            BuffKind::Rooted => vec![BuffEffect::MovementSpeed(0.0)],
            BuffKind::Winded => vec![
                BuffEffect::MovementSpeed(1.0 - nn_scaling_custom(data.strength, 1.0)),
                BuffEffect::EnergyReward(1.0 - nn_scaling(data.strength)),
            ],
            BuffKind::Amnesia => vec![BuffEffect::DisableAuxiliaryAbilities],
            BuffKind::OffBalance => vec![BuffEffect::PoiseReduction(-data.strength)],
            BuffKind::Tenacity => vec![
                BuffEffect::DamageReduction(nn_scaling(data.strength) / 2.0),
                BuffEffect::MovementSpeed(0.7),
                BuffEffect::DamagedEffect(StatEffect::new(
                    StatEffectTarget::Target,
                    CombatEffect::Energy(data.strength * 10.0),
                )),
            ],
            BuffKind::Resilience => vec![BuffEffect::CrowdControlResistance(data.strength)],
            BuffKind::StormChaser => {
                vec![BuffEffect::AttackEffect(
                    AttackEffect::new(
                        None,
                        CombatEffect::SelfBuff(CombatBuff {
                            kind: BuffKind::Hastened,
                            dur_secs: data.secondary_duration.unwrap_or(Secs(1.0)),
                            strength: CombatBuffStrength::Value(data.strength),
                            chance: 1.0,
                        }),
                    )
                    .with_requirement(CombatRequirement::AttackSource(AttackSource::Projectile)),
                )]
            },
            BuffKind::EagleEye => {
                vec![
                    BuffEffect::PrecisionModifier(
                        Some(CombatRequirement::AttackSource(AttackSource::Projectile)),
                        data.strength,
                        false,
                    ),
                    BuffEffect::PrecisionPowerMult(1.0 + data.strength * 0.5),
                    BuffEffect::EnergyReward(0.5),
                ]
            },
            BuffKind::OwlTalon => vec![
                BuffEffect::PrecisionModifier(Some(CombatRequirement::TargetUnwielded), 0.8, false),
                BuffEffect::AttackDamage(1.0 + data.strength),
            ],
            BuffKind::HeavyNock => {
                let range_mod = CombatModification::RangeWeakening {
                    start_dist: 5.0,
                    end_dist: 50.0,
                    min_str: 0.3,
                };
                let poise = AttackEffect::new(None, CombatEffect::Poise(35.0 * data.strength))
                    .with_requirement(CombatRequirement::AnyDamage)
                    .with_requirement(CombatRequirement::AttackSource(AttackSource::Projectile))
                    .with_modification(range_mod);
                vec![
                    BuffEffect::KnockbackMult(data.strength * 5.0),
                    BuffEffect::AttackEffect(poise),
                    BuffEffect::AttackDamage(0.75), // TODO: has no effect on damage?
                ]
            },
            BuffKind::Heartseeker => {
                let energy =
                    AttackEffect::new(None, CombatEffect::EnergyReward(14.0 * data.strength))
                        .with_requirement(CombatRequirement::AnyDamage)
                        .with_requirement(CombatRequirement::AttackSource(
                            AttackSource::Projectile,
                        ));
                vec![
                    BuffEffect::PrecisionModifier(
                        Some(CombatRequirement::AttackSource(AttackSource::Projectile)),
                        data.strength * 1.2,
                        false,
                    ),
                    BuffEffect::AttackEffect(energy),
                ]
            },
            BuffKind::ArdentHunter => vec![BuffEffect::AttackEffect(
                AttackEffect::new(
                    None,
                    CombatEffect::Buff(CombatBuff {
                        kind: BuffKind::ArdentHunted,
                        dur_secs: data.secondary_duration.unwrap_or(Secs(60.0)),
                        strength: CombatBuffStrength::Value(data.strength),
                        chance: 1.0,
                    }),
                )
                .with_requirement(CombatRequirement::AttackSource(AttackSource::Projectile)),
            )],
            BuffKind::SepticShot => vec![BuffEffect::AttackEffect(
                AttackEffect::new(None, CombatEffect::DebuffsVulnerable {
                    mult: data.strength,
                    scaling: ScalingKind::Sqrt,
                    filter_attacker: true,
                    filter_weapon: Some(ToolKind::Bow),
                })
                .with_requirement(CombatRequirement::AttackSource(AttackSource::Projectile)),
            )],
            BuffKind::ArdentHunt => {
                const GENERAL_MULT: f32 = 0.75;
                let hunt_mult = (1.0 / GENERAL_MULT.max(0.1)) + data.strength;
                let mut effects = vec![BuffEffect::AttackDamage(GENERAL_MULT)];
                if let Some(target) = target_entity {
                    effects.push(BuffEffect::AttackEffect(
                        AttackEffect::new(None, CombatEffect::AdditionalDamage(hunt_mult - 1.0))
                            .with_requirement(CombatRequirement::Target(target)),
                    ));
                    effects.push(BuffEffect::MarkEntity(target));
                }
                effects
            },
            BuffKind::Terrified => {
                // BL-05 Fear rider, redesigned on the BL-52 combat-resolution
                // engine: slowed AND fights at a disadvantage. The flee behaviour
                // lives in the agent AI (`is_terrified`); the disadvantage is a
                // flat **−accuracy** (more misses, same damage when it lands) —
                // not a damage cut. −12 acc ≈ −18% hit vs an equal-evasion target
                // (balance note §8); scales with buff strength.
                vec![
                    BuffEffect::MovementSpeed(1.0 - nn_scaling(data.strength)),
                    BuffEffect::Accuracy(-12.0 * nn_scaling(data.strength)),
                ]
            },
            // State tags, not stat modifiers — their consequences are
            // enforced in the agent behaviour tree and combat's attack
            // resolution, neither of which a BuffEffect can reach.
            BuffKind::Charmed | BuffKind::Dominated | BuffKind::Maddened => vec![],
            BuffKind::Hollowtouched => vec![BuffEffect::MaxHealthModifier {
                value: 1.0 - (0.08 * data.strength).min(0.4),
                kind: ModifierKind::Multiplicative,
            }],
            BuffKind::IgniteArrow => vec![
                BuffEffect::ProjectileConstructorEffect(ProjectileConstructorEffect {
                    kind: ProjectileConstructorEffectKind::AttackEffect(
                        AttackEffect::new(
                            None,
                            CombatEffect::Buff(CombatBuff {
                                kind: BuffKind::Burning,
                                dur_secs: data.secondary_duration.unwrap_or(Secs(10.0)),
                                strength: CombatBuffStrength::DamageFraction(data.strength),
                                chance: 1.0,
                            }),
                        )
                        .with_requirement(CombatRequirement::AttackSource(
                            AttackSource::Projectile,
                        )),
                    ),
                    tool_filter: Some(ToolKind::Bow),
                }),
                BuffEffect::ProjectileConstructorEffect(ProjectileConstructorEffect {
                    kind: ProjectileConstructorEffectKind::Marker(FrontendMarker::IgniteArrow),
                    tool_filter: Some(ToolKind::Bow),
                }),
            ],
            BuffKind::FreezeArrow => vec![
                BuffEffect::ProjectileConstructorEffect(ProjectileConstructorEffect {
                    kind: ProjectileConstructorEffectKind::AttackEffect(
                        AttackEffect::new(
                            None,
                            CombatEffect::Buff(CombatBuff {
                                kind: BuffKind::Frozen,
                                dur_secs: data.secondary_duration.unwrap_or(Secs(10.0)),
                                strength: CombatBuffStrength::Value(data.strength),
                                chance: 1.0,
                            }),
                        )
                        .with_requirement(CombatRequirement::AttackSource(
                            AttackSource::Projectile,
                        )),
                    ),
                    tool_filter: Some(ToolKind::Bow),
                }),
                BuffEffect::ProjectileConstructorEffect(ProjectileConstructorEffect {
                    kind: ProjectileConstructorEffectKind::Marker(FrontendMarker::FreezeArrow),
                    tool_filter: Some(ToolKind::Bow),
                }),
            ],
            BuffKind::DrenchArrow => vec![
                BuffEffect::ProjectileConstructorEffect(ProjectileConstructorEffect {
                    kind: ProjectileConstructorEffectKind::AttackEffect(
                        AttackEffect::new(
                            None,
                            CombatEffect::Buff(CombatBuff {
                                kind: BuffKind::Poisoned,
                                dur_secs: data.secondary_duration.unwrap_or(Secs(10.0)),
                                strength: CombatBuffStrength::Value(data.strength),
                                chance: 1.0,
                            }),
                        )
                        .with_requirement(CombatRequirement::AttackSource(
                            AttackSource::Projectile,
                        )),
                    ),
                    tool_filter: Some(ToolKind::Bow),
                }),
                BuffEffect::ProjectileConstructorEffect(ProjectileConstructorEffect {
                    kind: ProjectileConstructorEffectKind::Marker(FrontendMarker::DrenchArrow),
                    tool_filter: Some(ToolKind::Bow),
                }),
            ],
            BuffKind::JoltArrow => vec![
                BuffEffect::ProjectileConstructorEffect(ProjectileConstructorEffect {
                    kind: ProjectileConstructorEffectKind::ConvertKindToArcing(
                        ProjectileArcingProperties {
                            distance: 8.0,
                            arcs: 3,
                            min_delay: Secs(0.25),
                            max_delay: Secs(1.0),
                            targets_owner: false,
                        },
                    ),
                    tool_filter: Some(ToolKind::Bow),
                }),
                BuffEffect::ProjectileConstructorEffect(ProjectileConstructorEffect {
                    kind: ProjectileConstructorEffectKind::Marker(FrontendMarker::JoltArrow),
                    tool_filter: Some(ToolKind::Bow),
                }),
            ],
            BuffKind::BlindingSmite => vec![
                BuffEffect::AttackEffect(AttackEffect::new(
                    None,
                    CombatEffect::AdditionalDamage(data.strength),
                )),
                BuffEffect::AttackEffect(AttackEffect::new(
                    None,
                    CombatEffect::Buff(CombatBuff {
                        kind: BuffKind::Blinded,
                        dur_secs: data.secondary_duration.unwrap_or(Secs(5.0)),
                        strength: CombatBuffStrength::Value(0.5),
                        chance: 1.0,
                    }),
                )),
            ],
            BuffKind::BrandingSmite | BuffKind::DivineSmite => vec![BuffEffect::AttackEffect(
                AttackEffect::new(None, CombatEffect::AdditionalDamage(data.strength)),
            )],
            BuffKind::SearingSmite => vec![
                BuffEffect::AttackEffect(AttackEffect::new(
                    None,
                    CombatEffect::AdditionalDamage(data.strength),
                )),
                BuffEffect::AttackEffect(AttackEffect::new(
                    None,
                    CombatEffect::Buff(CombatBuff {
                        kind: BuffKind::Burning,
                        dur_secs: data.secondary_duration.unwrap_or(Secs(5.0)),
                        strength: CombatBuffStrength::DamageFraction(0.3),
                        chance: 1.0,
                    }),
                )),
            ],
            BuffKind::StaggeringSmite => vec![
                BuffEffect::AttackEffect(AttackEffect::new(
                    None,
                    CombatEffect::AdditionalDamage(data.strength),
                )),
                BuffEffect::AttackEffect(AttackEffect::new(None, CombatEffect::Poise(40.0))),
            ],
            BuffKind::ThunderousSmite => vec![
                BuffEffect::AttackEffect(AttackEffect::new(
                    None,
                    CombatEffect::AdditionalDamage(data.strength),
                )),
                BuffEffect::AttackEffect(AttackEffect::new(
                    None,
                    CombatEffect::Knockback(Knockback {
                        direction: KnockbackDir::Away,
                        strength: 8.0,
                    }),
                )),
            ],
            BuffKind::WrathfulSmite => vec![
                BuffEffect::AttackEffect(AttackEffect::new(
                    None,
                    CombatEffect::AdditionalDamage(data.strength),
                )),
                BuffEffect::AttackEffect(AttackEffect::new(
                    None,
                    CombatEffect::Buff(CombatBuff {
                        kind: BuffKind::Terrified,
                        dur_secs: data.secondary_duration.unwrap_or(Secs(5.0)),
                        strength: CombatBuffStrength::Value(1.0),
                        chance: 1.0,
                    }),
                )),
            ],
            BuffKind::Blessed => vec![BuffEffect::Accuracy(12.0 * nn_scaling(data.strength))],
            BuffKind::CrusadersMantle => vec![BuffEffect::AttackEffect(AttackEffect::new(
                None,
                CombatEffect::AdditionalDamage(data.strength),
            ))],
            // Area/generic active senses: membership frozen at cast (`SenseMode::Snapshot`).
            BuffKind::Detecting => {
                let mut effects = Vec::new();
                if let Some(MiscBuffData::Sense(kind, radius, mode)) = data.misc_data {
                    effects.push(BuffEffect::Sense { kind, radius, mode });
                }
                effects
            },
            // Concealment-piercing only; produces no reveal set of its own.
            // Unlike `Detecting`/`TrueSight`, this never reads `misc_data` --
            // there is nothing for it to carry, since `see_invisibility` is
            // not a reveal-set spell at all (its entire job is ignoring the
            // concealment modifier on the observer side).
            BuffKind::SeeInvisible => vec![BuffEffect::PierceConcealment],
            // Always-active, continuously re-evaluated (`SenseMode::Continuous`)
            // true sense, unconditionally piercing concealment, illusion and
            // darkness for as long as it lasts -- the one unified True Sight
            // mechanism. The reveal-set `Sense` effect is still conditional
            // on `misc_data` being present (same "omit rather than guess"
            // pattern every other sense-carrying `BuffKind` uses), but the
            // three pierce flags are unconditional: they are this buff's
            // defining behaviour, not something a caller could opt out of by
            // omitting `misc_data`.
            BuffKind::TrueSight => {
                let mut effects = Vec::new();
                if let Some(MiscBuffData::Sense(kind, radius, mode)) = data.misc_data {
                    effects.push(BuffEffect::Sense { kind, radius, mode });
                }
                effects.push(BuffEffect::PierceConcealment);
                effects.push(BuffEffect::PierceIllusion);
                effects.push(BuffEffect::PierceDarkness);
                effects
            },
            // Declares the shape of a remote-sensing link (which kind of
            // anchor, whether the caster may look around freely, whether they
            // may drive it). Deliberately carries no entity identity — see
            // `MiscBuffData::RemoteSense`'s doc comment. The link's actual
            // anchor `Uid` is written server-side, at cast time, into the
            // owner-private `RemoteSense` component, never here.
            BuffKind::RemoteSensing => {
                // Rooted: the caster's body cannot move at all while their
                // senses are anchored elsewhere (confirmed design: no damage
                // immunity and no aggro suppression accompany this — the body
                // stays a fully normal, hittable target). Same idiom as
                // `Asleep`'s immobilisation.
                let mut effects = vec![BuffEffect::MovementSpeed(0.0)];
                if let Some(MiscBuffData::RemoteSense {
                    anchor_kind,
                    free_look,
                    piloted,
                    ..
                }) = data.misc_data
                {
                    // `spawn_range`/`flight_speed` are deliberately not
                    // forwarded onto `BuffEffect::RemoteSense` -- that effect
                    // is a declared-but-inert marker
                    // (`common/systems/src/buff.rs` matches it as a no-op);
                    // the actual anchor mechanics read those two fields
                    // straight from `data.misc_data` below, at cast time.
                    effects.push(BuffEffect::RemoteSense {
                        anchor_kind,
                        free_look,
                        piloted,
                    });
                }
                effects
            },
            // Cosmetic-only disguise — see `BuffEffect::Disguise`'s doc
            // comment for why this must never become a `BodyChange`.
            // `cast_accuracy` is carried in `data.strength`: the caster's
            // live `magic_accuracy` is only available at cast time (in the
            // character-state code that builds this buff's `BuffData`, which
            // has the caster's own `Stats` in scope), not here, so it is
            // snapshotted into `strength` there the same way `RemoteSensing`
            // above snapshots cast-time-only data it can't derive from
            // `MiscBuffData` alone.
            BuffKind::Disguised => {
                let mut effects = Vec::new();
                if let Some(MiscBuffData::Body(apparent_body)) = data.misc_data
                    && let Some(caster) = source_entity
                {
                    effects.push(BuffEffect::Disguise {
                        apparent_body,
                        caster,
                        cast_accuracy: data.strength,
                    });
                }
                effects
            },
            // Concealment aura target buff: just contributes to the shared
            // stealth channel, no misc_data needed.
            BuffKind::PassWithoutTrace => vec![BuffEffect::Stealth(data.strength)],
            // Concealment aura target buff with a secondary magic-resistance
            // rider. Unlike `Frenzied`/`Fortitude`'s single-dial derivations
            // above, `strength` (stealth, a detection-avoidance stat) and the
            // resistance rider (AoE spell mitigation) measure unrelated
            // things, so the rider is its own authored value via
            // `misc_data` rather than a fixed ratio of `strength`.
            BuffKind::Mooncloak => {
                let mut effects = vec![BuffEffect::Stealth(data.strength)];
                if let Some(MiscBuffData::ResistMagic(resist)) = data.misc_data {
                    effects.push(BuffEffect::Resistance(ResistKind::Magic, resist));
                }
                effects
            },
            // Anti-divination flag, no other effect. `strength`/`duration`
            // govern nothing here beyond the buff's own lifetime.
            BuffKind::Nondetection => vec![BuffEffect::Nondetection],
            // `magic_aura`'s lie: reports a match for exactly the authored
            // `SenseKind`, or no effect at all if the RON forgot to author
            // one (same "omit rather than silently derive" guard
            // `Mooncloak` uses for its own optional rider above).
            BuffKind::MagicAura => {
                let mut effects = Vec::new();
                if let Some(MiscBuffData::FalseAura(kind)) = data.misc_data {
                    effects.push(BuffEffect::FalseAura(kind));
                }
                effects
            },
            // Both anti-perception primitives at once: unconditional
            // anti-divination plus a large stealth contribution meant to
            // push the target's detection multiplier near zero. See
            // `sequester.ron` for the chosen strength and its resulting
            // multiplier.
            BuffKind::Sequester => {
                vec![BuffEffect::Nondetection, BuffEffect::Stealth(data.strength)]
            },
        }
    }

    fn extend_cat_ids(&self, mut cat_ids: Vec<BuffCategory>) -> Vec<BuffCategory> {
        match self {
            BuffKind::PotionSickness => {
                cat_ids.push(BuffCategory::PersistOnDowned);
            },
            // Every remote-sensing link is sustained by concentration (only
            // one at a time; a solid hit ends it) and joined cheaply by the
            // server's remote-sense system via its own category — tag both
            // here so RON authors can't forget one.
            BuffKind::RemoteSensing => {
                cat_ids.push(BuffCategory::Concentration);
                cat_ids.push(BuffCategory::RemoteSense);
            },
            _ => {},
        }
        cat_ids
    }

    fn modify_data(
        &self,
        mut data: BuffData,
        source_mass: Option<&Mass>,
        dest_info: DestInfo,
        source: BuffSource,
    ) -> BuffData {
        // TODO: Remove clippy allow after another buff needs this
        #[expect(clippy::single_match)]
        match self {
            BuffKind::Rooted => {
                let source_mass = source_mass.map_or(50.0, |m| m.0);
                let dest_mass = dest_info.mass.map_or(50.0, |m| m.0);
                let low_clamp = (0.25 + data.strength * 0.25).clamp(0.0, 1.0);
                let high_clamp = (1.0 + data.strength * 0.5).max(1.0);
                let ratio = (source_mass / dest_mass).clamp(low_clamp, high_clamp);
                data.duration = data.duration.map(|dur| Secs(dur.0 * ratio as f64));
            },
            _ => {},
        }
        if self.resilience_ccr_strength(data).is_some() {
            let dur_mult = dest_info
                .stats
                .map_or(1.0, |s| (1.0 - s.crowd_control_resistance).max(0.0));
            data.duration = data.duration.map(|dur| dur * dur_mult as f64);
        }
        self.apply_item_effect_reduction(&mut data, source, dest_info);
        data
    }

    /// If a buff kind should also give resilience when applied, return the
    /// strength that resilience should have, otherwise return None
    pub fn resilience_ccr_strength(&self, data: BuffData) -> Option<f32> {
        match_some!(self,
            BuffKind::Amnesia => 0.3,
            BuffKind::Frozen => data.strength,
            BuffKind::Winded => data.strength / 3.0,
            BuffKind::Rooted => data.duration.map_or(0.1, |dur| dur.0 as f32 / 10.0),
            // Automatic diminishing returns via the shipped Resilience grant —
            // the safeguard that stops repeated re-application from becoming
            // a permanent stun-lock.
            BuffKind::Paralyzed => data.strength,
        )
    }

    pub fn apply_item_effect_reduction(
        &self,
        data: &mut BuffData,
        source: BuffSource,
        dest_info: DestInfo,
    ) {
        if !matches!(source, BuffSource::Item) {
            return;
        }
        let item_effect_reduction = dest_info.stats.map_or(1.0, |s| s.item_effect_reduction);
        match self {
            BuffKind::Potion | BuffKind::Agility => {
                data.strength *= item_effect_reduction;
            },
            BuffKind::Burning | BuffKind::Frozen | BuffKind::Resilience => {
                data.duration = data.duration.map(|dur| dur * item_effect_reduction as f64);
            },
            _ => {},
        };
    }
}

// Struct used to store data relevant to a buff
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct BuffData {
    pub strength: f32,
    #[serde(default)]
    pub duration: Option<Secs>,
    #[serde(default)]
    pub delay: Option<Secs>,
    /// Used for buffs that have rider buffs (e.g. Flame, Frigid)
    #[serde(default)]
    pub secondary_duration: Option<Secs>,
    /// Used to add random data to buffs if needed (e.g. polymorphed)
    #[serde(default)]
    pub misc_data: Option<MiscBuffData>,
}

impl Default for BuffData {
    fn default() -> Self { Self::new(0.0, Some(Secs(0.0))) }
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub enum MiscBuffData {
    Body(Body),
    /// Parameters for a `BuffEffect::Sense`: which sense, its radius, and
    /// whether it snapshots once or re-evaluates continuously.
    Sense(SenseKind, f32, SenseMode),
    /// Parameters for a `BuffEffect::RemoteSense`: the shape of a
    /// remote-sensing link this buff sustains.
    ///
    /// 🔴 Deliberately carries no `Uid` and no world position. `BuffData` (via
    /// `Buff`/`Buffs`) is `SyncFrom::AnyEntity` — anyone nearby can see that a
    /// buff is active — so embedding the anchor's identity here would
    /// broadcast exactly the "who is watching what" leak the owner-private
    /// `RemoteSense` component exists to prevent. The resolved anchor lives
    /// only on `RemoteSense`, which is `SyncFrom::ClientEntity`.
    RemoteSense {
        anchor_kind: SenseAnchorKind,
        free_look: bool,
        piloted: bool,
        /// How far in front of the caster's own facing a `Piloted` anchor
        /// spawns, in blocks (`server/src/events/remote_sense.rs`'s
        /// `resolve_piloted`). Meaningless for `Existing`/`Sensor` anchors,
        /// which never compute a caster-relative spawn point at all --
        /// `Sensor` spawns at the cast's own target point, and `Existing`
        /// doesn't spawn anything. Defaults to `arcane_eye`'s own shipped
        /// value so `clairvoyance.ron`/`beast_sense.ron` (neither of which
        /// uses a `Piloted` anchor) don't need updating.
        #[serde(default = "default_piloted_spawn_range")]
        spawn_range: f32,
        /// Horizontal/vertical cruise speed of a `Piloted` anchor, in blocks
        /// per second, read every tick by `common/systems/src/pilot.rs` off
        /// the caster's own `RemoteSense` component (which carries this
        /// value forward from cast time -- see
        /// `common/src/comp/remote_sense.rs`). Meaningless for
        /// `Existing`/`Sensor` anchors, same defaulting rationale as
        /// `spawn_range` above.
        #[serde(default = "default_piloted_flight_speed")]
        flight_speed: f32,
        /// How far behind a `Tracking` anchor's watched target its sensor
        /// floats, in blocks (`server/src/sys/remote_sense.rs`'s
        /// `scrying_follow_offset`). Meaningless for every other anchor kind.
        /// Defaults to `scrying`'s own shipped value so
        /// `arcane_eye.ron`/`clairvoyance.ron`/`beast_sense.ron` (none of
        /// which uses a `Tracking` anchor) don't need updating.
        #[serde(default = "default_tracking_behind_dist")]
        behind_dist: f32,
        /// How far above a `Tracking` anchor's watched target its sensor
        /// floats, in blocks. Same defaulting rationale as `behind_dist`
        /// above.
        #[serde(default = "default_tracking_above_dist")]
        above_dist: f32,
    },
    /// A secondary `BuffEffect::Resistance(ResistKind::Magic, ..)` magnitude,
    /// authored independently of a buff's primary `strength` field. Used by
    /// `BuffKind::Mooncloak`, whose stealth strength and magic-resistance
    /// rider measure unrelated things (detection avoidance vs. AoE spell
    /// mitigation) and should not be coupled to a single dial.
    ResistMagic(f32),
    /// The `SenseKind` a `BuffKind::MagicAura` should lie about, authored
    /// independently of `strength` for the same reason `ResistMagic` above
    /// is: which sense to fake isn't a magnitude on any existing dial. The
    /// mechanism itself (`BuffEffect::FalseAura`) is general over every
    /// `SenseKind`; RON content authors `Magic` for `magic_aura`'s own
    /// flavor (feeding false readings to a `detect magic`-style query), but
    /// nothing in code hardcodes that choice.
    FalseAura(SenseKind),
}

/// `arcane_eye`'s own shipped spawn range
/// (`server/src/events/remote_sense.rs`'s old `EYE_SPAWN_RANGE` const, before
/// this became a RON-authored field), kept as the serde default so every other
/// `MiscBuffData::RemoteSense`-using RON file doesn't need to opt in
/// explicitly.
fn default_piloted_spawn_range() -> f32 { 9.0 }

/// `arcane_eye`'s own shipped flight speed (`common/systems/src/pilot.rs`'s
/// old `EYE_FLIGHT_SPEED` const, before this became a RON-authored field),
/// same defaulting rationale as `default_piloted_spawn_range` above.
fn default_piloted_flight_speed() -> f32 { 6.0 }

/// `scrying`'s own shipped behind-distance
/// (`server/src/sys/remote_sense.rs`'s old `SCRY_SENSOR_BEHIND_DIST` const,
/// before this became a RON-authored field), same defaulting rationale as
/// `default_piloted_spawn_range` above.
fn default_tracking_behind_dist() -> f32 { 3.0 }

/// `scrying`'s own shipped above-distance (formerly `SCRY_SENSOR_ABOVE_DIST`),
/// same defaulting rationale as `default_tracking_behind_dist` above.
fn default_tracking_above_dist() -> f32 { 2.0 }

/// Which kind of `SenseAnchor` a `RemoteSensing` buff declares, without the
/// anchor's identity (see `MiscBuffData::RemoteSense`'s doc comment for why
/// the `Uid` itself is never carried here).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum SenseAnchorKind {
    Existing,
    Sensor,
    Piloted,
    /// `scrying`: a spawned sensor, like `Sensor`, but one that follows a
    /// specific targeted creature (`ResolveRemoteSenseEvent::target_entity`)
    /// instead of sitting at a fixed point, and whose link is subject to a
    /// periodic resisted-magic-effect re-roll against that creature rather
    /// than only a range/aliveness check. See `SenseAnchor::Tracking`.
    Tracking,
}

/// Whether a sense's reveal set is computed once and frozen for the buff's
/// lifetime, or continuously re-evaluated while the buff is active.
///
/// `Continuous` is deliberately rare: it is reserved for a sense that is
/// genuinely always-active for as long as it lasts (entities glow the instant
/// they come into range and stop the instant they leave), not a periodic
/// re-scan. Every other sense uses `Snapshot`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum SenseMode {
    Snapshot,
    Continuous,
}

impl BuffData {
    pub fn new(strength: f32, duration: Option<Secs>) -> Self {
        Self {
            strength,
            duration,
            delay: None,
            secondary_duration: None,
            misc_data: None,
        }
    }

    pub fn with_delay(mut self, delay: Secs) -> Self {
        self.delay = Some(delay);
        self
    }

    pub fn with_secondary_duration(mut self, sec_dur: Secs) -> Self {
        self.secondary_duration = Some(sec_dur);
        self
    }

    pub fn with_misc_data(mut self, misc_data: MiscBuffData) -> Self {
        self.misc_data = Some(misc_data);
        self
    }
}

/// De/buff category ID.
/// Similar to `BuffKind`, but to mark a category (for more generic usage, like
/// positive/negative buffs).
#[derive(Clone, Eq, PartialEq, Debug, Serialize, Deserialize)]
pub enum BuffCategory {
    Natural,
    Physical,
    Magical,
    Divine,
    PersistOnDowned,
    PersistOnDeath,
    FromActiveAura(Uid, AuraKey),
    FromLink(DynWeakLinkHandle),
    /// Buffs with this category are removed in the EntityAttackedHook event
    /// handler
    RemoveOnAttack,
    /// Buffs with this category are removed by any loadout changes triggered
    /// from the character state system, buffs added by the self buff character
    /// state automatically have this buff category
    RemoveOnLoadoutChange,
    /// Ensures only 1 buff with this category can be present on an entity,
    /// enforced in self buff character state
    // TODO: Do we want to enforce this category similarly to how WeaponCoating is enforced?
    SelfBuff,
    /// Ensures only 1 buff with this category can be present on an entity,
    /// enforced in buff event handling. Currently cleared in the event handler
    /// for shooting a projectile.
    // TODO: If we need to clear buffs with this category in another place (e.g. on melee attacks),
    // then logic will need to be added.
    WeaponCoating,
    /// Sustained by concentration (ENG-C2 / M5): only one such buff is held at
    /// a time (a new one removes the prior), and it is removed when the
    /// bearer takes a hit at or above the break threshold. Tag
    /// concentration-spell buffs/auras with this.
    Concentration,
    /// Tags a buff as maintaining an active magical sense (see
    /// `BuffEffect::Sense`). Lets a detection query join cheaply over only
    /// entities with an active sense, and lets a future dispel target
    /// detections as a class.
    Detection,
    /// Tags a buff as maintaining an active remote-sensing link (see
    /// `RemoteSense`). Lets the server's remote-sense system join cheaply
    /// over only the handful of entities with an active link, instead of
    /// scanning every buff on every entity. Always paired with
    /// `Concentration` (see `BuffKind::RemoteSensing::extend_cat_ids`).
    RemoteSense,
}

/// Concentration break threshold = base + a fraction of the bearer's **max
/// HP**. Because max HP grows with level/skills, a more powerful character
/// resists more before a hit breaks concentration (ENG-C2 / M5, Matias §6.5: "a
/// medida que sube de nivel resiste más"). Tunable and kept modest so it stays
/// playable — refine with `game-balance-designer` when content lands.
/// Self-inflicted costs (e.g. the Hemomancy HP price, `cause: None`) do NOT
/// break it: the check only fires for hits with a `DamageSource` cause.
pub const CONCENTRATION_BREAK_BASE: f32 = 10.0;
pub const CONCENTRATION_BREAK_HP_FRACTION: f32 = 0.1;

/// The single-hit damage needed to break the concentration of a bearer whose
/// maximum health is `max_hp`.
pub fn concentration_break_threshold(max_hp: f32) -> f32 {
    CONCENTRATION_BREAK_BASE + max_hp.max(0.0) * CONCENTRATION_BREAK_HP_FRACTION
}

/// Whether a single hit of `damage` (a positive amount) breaks the
/// concentration of a bearer with `max_hp` maximum health.
pub fn concentration_breaks(damage: f32, max_hp: f32) -> bool {
    damage >= concentration_break_threshold(max_hp)
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub enum ModifierKind {
    Additive,
    Multiplicative,
}

/// Data indicating and configuring behaviour of a de/buff.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub enum BuffEffect {
    /// Periodically damages or heals entity
    HealthChangeOverTime {
        rate: f32,
        kind: ModifierKind,
        instance: u64,
        tick_dur: Secs,
    },
    /// Periodically consume entity energy
    EnergyChangeOverTime {
        rate: f32,
        kind: ModifierKind,
        tick_dur: Secs,
        reset_rate_on_tick: bool,
    },
    /// Periodically change entity combo
    ComboChangeOverTime {
        rate: f32,
        tick_dur: Secs,
    },
    /// Changes maximum health by a certain amount
    MaxHealthModifier {
        value: f32,
        kind: ModifierKind,
    },
    /// Changes maximum energy by a certain amount
    MaxEnergyModifier {
        value: f32,
        kind: ModifierKind,
    },
    /// Reduces damage after armor is accounted for by this fraction
    DamageReduction(f32),
    /// Gradually changes an entities max health over time
    MaxHealthChangeOverTime {
        rate: f32,
        kind: ModifierKind,
        target_fraction: f32,
    },
    /// Modifies move speed of target
    MovementSpeed(f32),
    /// Modifies charging move speed of target
    ChargeMoveSpeed(f32),
    /// Modifies buildup move speed of target
    BuildupMoveSpeed(f32),
    /// Modifies attack speed of target
    AttackSpeed(f32),
    /// Modifies recovery speed of target
    RecoverySpeed(f32),
    /// Modifies charging speed of target
    ChargingSpeed(f32),
    /// Modifies buildup speed of target
    BuildupSpeed(f32),
    /// Modifies ground friction of target
    GroundFriction(f32),
    /// Reduces poise damage taken after armor is accounted for by this fraction
    PoiseReduction(f32),
    /// Increases poise damage dealt when health is lost
    PoiseDamageFromLostHealth(f32),
    /// Modifier to the amount of damage dealt with attacks
    AttackDamage(f32),
    /// Adds a precision modifier applied to an attack if the condition
    /// is met, also allows for the modifier to optionally override other
    /// precision bonuses
    PrecisionModifier(Option<CombatRequirement>, f32, bool),
    /// Overrides the precision multiplier applied to an incoming attack
    PrecisionVulnerabilityOverride(f32),
    /// Changes body.
    BodyChange(Body),
    BuffImmunity(BuffKind),
    SwimSpeed(f32),
    /// Add an attack effect to attacks made while buff is active
    AttackEffect(AttackEffect),
    /// Increases poise damage dealt by attacks
    AttackPoise(f32),
    /// Ignores some damage reduction on target
    MitigationsPenetration(f32),
    /// Modifies energy rewarded on successful strikes
    EnergyReward(f32),
    /// Modifies energy efficiency of using abilities
    EnergyEfficiency(f32),
    /// Add an effect to the entity when damaged by an attack
    DamagedEffect(StatEffect),
    /// Add an effect to the entity when killed
    DeathEffect(StatEffect),
    /// Prevents use of auxiliary abilities
    DisableAuxiliaryAbilities,
    /// Antimagic (BL-36): prevents activation of magic abilities and suppresses
    /// attuned magic-item effects. Sets `Stats.disable_magic`.
    DisableMagic,
    /// Dimensional anchor (BL-05): prevents teleport/blink. Sets
    /// `Stats.disable_teleport`.
    DisableTeleport,
    /// Grants the buffed entity the ability to see through any amount of
    /// another entity's concealment, regardless of that entity's stealth
    /// value. Sets `Stats.pierce_concealment`, read on the observer's side
    /// by `combat::perception_dist_multiplier_from_stealth`.
    PierceConcealment,
    /// Grants the buffed entity the ability to see through illusions and
    /// disguises unconditionally. Sets `Stats.pierce_illusion`, read by
    /// `server/agent/src/action_nodes.rs`'s `true_sight_pierces_disguise` as
    /// an early-out before the disguise suspicion roll.
    PierceIllusion,
    /// Grants the buffed entity the ability to see through mundane and
    /// magical darkness. Sets `Stats.pierce_darkness` — a flag only, its
    /// lighting-override consumer is a separate future addition.
    PierceDarkness,
    /// Anti-divination (`nondetection`): the buffed entity can't be targeted
    /// by any divination sense, unconditionally. Sets `Stats.nondetection`,
    /// read by `server/src/sys/detection.rs`'s per-`SenseKind` predicate as
    /// an early-out.
    Nondetection,
    /// `magic_aura`'s lie: the buffed entity registers as revealed by the
    /// given `SenseKind` regardless of whether it actually matches that
    /// sense's real predicate. Sets `Stats.false_aura`, read by the same
    /// predicate `Nondetection` above short-circuits.
    FalseAura(SenseKind),
    /// Combat resolution (BL-52): flat additive modifier to the buffed entity's
    /// physical to-hit accuracy (negative = harder to land, e.g. Fear). Sets
    /// `Stats.accuracy`.
    Accuracy(f32),
    /// Combat resolution (BL-52): flat additive modifier to the buffed entity's
    /// evasion (harder to be hit). Sets `Stats.evasion`.
    Evasion(f32),
    /// Additive contribution to the buffed entity's stealth, summed with
    /// item-based stealth in the same `1/(1+sum)` curve by
    /// `combat::perception_dist_multiplier_from_stealth`. Sets
    /// `Stats.stealth`.
    Stealth(f32),
    /// Combat resolution (BL-52): flat additive modifier to the buffed entity's
    /// critical-hit chance. Sets `Stats.crit_chance`.
    CritChance(f32),
    /// Combat resolution (BL-52 P3): adds to a typed elemental resistance
    /// channel (fraction), mitigating incoming **AoE** damage of that element.
    /// Sets the matching `Stats.resist_*`.
    Resistance(ResistKind, f32),
    /// Reduces duration of crowd control debuffs
    CrowdControlResistance(f32),
    /// Reduces the strength or duration of item buff
    ItemEffectReduction(f32),
    /// Adds an effect that modifies how attacks are applied to this entity
    AttackedModification(AttackedModification),
    /// Multiplies the precision damage applied to attacks made
    PrecisionPowerMult(f32),
    /// Multiplies knockback dealt by attacks
    KnockbackMult(f32),
    /// Multiplier to speed of projectiles fired by the target
    ProjectileSpeedMult(f32),
    /// Adds effects to projectiles when they are constructed from a projectile
    /// constructor
    ProjectileConstructorEffect(ProjectileConstructorEffect),
    MarkEntity(Uid),
    /// Declares an active magical sense of `kind`, evaluated within `radius`,
    /// using `mode` to decide whether it snapshots once or re-evaluates
    /// continuously. Sets `Stats.senses`; the actual reveal set is computed by
    /// a spatial query elsewhere and stored on the owner-private `Detected`
    /// component, never here.
    Sense {
        kind: SenseKind,
        radius: f32,
        mode: SenseMode,
    },
    /// Declares the shape of an active remote-sensing link: which kind of
    /// anchor it is, whether the bearer may look around freely from it, and
    /// whether the bearer may drive it. Carries no entity identity (see
    /// `MiscBuffData::RemoteSense`) — the resolved anchor `Uid` lives only on
    /// the owner-private `RemoteSense` component, written server-side at cast
    /// time, never derived from this effect.
    RemoteSense {
        anchor_kind: SenseAnchorKind,
        free_look: bool,
        piloted: bool,
    },
    /// Declares a cosmetic-only disguise: the `Body` observers should be
    /// shown instead of the bearer's real one, who cast it, and a snapshot
    /// of the caster's `magic_accuracy` at cast time. 🔴 Deliberately does
    /// **not** change the bearer's actual `Body`/`Mass`/`Density`/`Collider`
    /// — contrast `BodyChange` above, which does. Sets/updates the
    /// `Disguise` component (`common/src/comp/disguise.rs`), never
    /// `ChangeBodyEvent`.
    Disguise {
        apparent_body: Body,
        caster: Uid,
        cast_accuracy: f32,
    },
}

/// Actual de/buff.
/// Buff can timeout after some time if `time` is Some. If `time` is None,
/// Buff will last indefinitely, until removed manually (by some action, like
/// uncursing).
///
/// Buff has a kind, which is used to determine the effects in a builder
/// function.
///
/// To provide more classification info when needed,
/// buff can be in one or more buff category.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Buff {
    pub kind: BuffKind,
    pub data: BuffData,
    pub cat_ids: Vec<BuffCategory>,
    pub end_time: Option<Time>,
    pub start_time: Time,
    pub effects: Vec<BuffEffect>,
    pub source: BuffSource,
    /// The magic source of the ability that caused this buff, if any —
    /// mirrors `HealthChange.magic_source`, threaded the same way
    /// (`combat.rs`'s `CombatBuff::to_buff`/`to_self_buff`, from
    /// `AbilityInfo.ability_meta.source`). `None` for world/item/aura/mount/
    /// command-sourced buffs, which have no casting ability to attribute.
    #[serde(default)]
    pub magic_source: Option<MagicSource>,
}

/// Information about whether buff addition or removal was requested.
/// This to implement "on_add" and "on_remove" hooks for constant buffs.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum BuffChange {
    /// Adds this buff.
    Add(Buff),
    /// Removes all buffs with this ID.
    RemoveByKind(BuffKind),
    /// Removes all buffs with this ID, but not debuffs.
    RemoveFromController(BuffKind),
    /// Removes buffs of these indices, should only be called when buffs expire
    RemoveByKey(Vec<BuffKey>),
    /// Removes buffs of these categories (first vec is of categories of which
    /// all are required, second vec is of categories of which at least one is
    /// required, third vec is of categories that will not be removed)
    RemoveByCategory {
        all_required: Vec<BuffCategory>,
        any_required: Vec<BuffCategory>,
        none_required: Vec<BuffCategory>,
    },
    /// Refreshes durations of all buffs with this kind.
    Refresh(BuffKind),
}

impl Buff {
    /// Builder function for buffs
    pub fn new(
        kind: BuffKind,
        data: BuffData,
        cat_ids: Vec<BuffCategory>,
        source: BuffSource,
        time: Time,
        dest_info: DestInfo,
        // Create source_info if we need more parameters from source
        source_mass: Option<&Mass>,
        // Note: This refers to the target of the ability that caused a new buff, which is not
        // necessarily the target of the buff
        target_uid: Option<Uid>,
        // The magic source of the ability that caused this buff, if any. See the field doc
        // comment on `Buff::magic_source`.
        magic_source: Option<MagicSource>,
    ) -> Self {
        let data = kind.modify_data(data, source_mass, dest_info, source);
        let source_uid = if let BuffSource::Character { by, .. } = source {
            Some(by)
        } else {
            None
        };
        let effects = kind.effects(&data, target_uid, source_uid);
        let cat_ids = kind.extend_cat_ids(cat_ids);
        let start_time = Time(time.0 + data.delay.map_or(0.0, |delay| delay.0));
        let end_time = if cat_ids.iter().any(|cat_id| {
            matches!(
                cat_id,
                BuffCategory::FromActiveAura(..) | BuffCategory::FromLink(_)
            )
        }) {
            None
        } else {
            data.duration.map(|dur| Time(start_time.0 + dur.0))
        };
        Buff {
            kind,
            data,
            cat_ids,
            start_time,
            end_time,
            effects,
            source,
            magic_source,
        }
    }

    /// Calculate how much time has elapsed since the buff was applied
    pub fn elapsed(&self, time: Time) -> Secs { Secs(time.0 - self.start_time.0) }
}

impl PartialOrd for Buff {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        if self == other {
            Some(Ordering::Equal)
        } else if self.data.strength > other.data.strength {
            Some(Ordering::Greater)
        } else if self.data.strength < other.data.strength {
            Some(Ordering::Less)
        } else if self.data.delay.is_none() && other.data.delay.is_some() {
            Some(Ordering::Greater)
        } else if self.data.delay.is_some() && other.data.delay.is_none() {
            Some(Ordering::Less)
        } else if compare_end_time(self.end_time, other.end_time) {
            Some(Ordering::Greater)
        } else if compare_end_time(other.end_time, self.end_time) {
            Some(Ordering::Less)
        } else {
            None
        }
    }
}

fn compare_end_time(a: Option<Time>, b: Option<Time>) -> bool {
    a.is_none_or(|time_a| b.is_some_and(|time_b| time_a.0 > time_b.0))
}

impl PartialEq for Buff {
    fn eq(&self, other: &Self) -> bool {
        self.data.strength == other.data.strength
            && self.end_time == other.end_time
            && self.start_time == other.start_time
    }
}

/// Source of the de/buff
#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub enum BuffSource {
    /// Applied by a character
    Character {
        by: Uid,
        tool_kind: Option<ToolKind>,
    },
    /// Applied by world, like a poisonous fumes from a swamp
    World,
    /// Applied by command
    Command,
    /// Applied by an item
    Item,
    /// Applied by another buff (like an after-effect)
    Buff,
    /// Applied by a block
    Block,
    /// Some other source
    Unknown,
}

/// Component holding all de/buffs that gets resolved each tick.
/// On each tick, remaining time of buffs get lowered and
/// buff effect of each buff is applied or not, depending on the `BuffEffect`
/// (specs system will decide based on `BuffEffect`, to simplify
/// implementation). TODO: Something like `once` flag for `Buff` to remove the
/// dependence on `BuffEffect` enum?
///
/// In case of one-time buffs, buff effects will be applied on addition
/// and undone on removal of the buff (by the specs system).
/// Example could be decreasing max health, which, if repeated each tick,
/// would be probably an undesired effect).
#[derive(Clone, Debug, Serialize, Deserialize, Default)]
pub struct Buffs {
    /// Maps kinds of buff to currently applied buffs of that kind and
    /// the time that the first buff was added (time gets reset if entity no
    /// longer has buffs of that kind)
    pub kinds: EnumMap<BuffKind, Option<(Vec<BuffKey>, Time)>>,
    // All buffs currently present on an entity
    pub buffs: SlotMap<BuffKey, Buff>,
}

impl Buffs {
    fn sort_kind(&mut self, kind: BuffKind) {
        if let Some(buff_order) = self.kinds[kind].as_mut() {
            if buff_order.0.is_empty() {
                self.kinds[kind] = None;
            } else {
                let buffs = &self.buffs;
                // Intentionally sorted in reverse so that the strongest buffs are earlier in
                // the vector
                buff_order
                    .0
                    .sort_by(|a, b| buffs[*b].partial_cmp(&buffs[*a]).unwrap_or(Ordering::Equal));
            }
        }
    }

    pub fn remove_kind(&mut self, kind: BuffKind) {
        if let Some((buff_keys, _)) = self.kinds[kind].as_ref() {
            for key in buff_keys {
                self.buffs.remove(*key);
            }
            self.kinds[kind] = None;
        }
    }

    pub fn insert(&mut self, buff: Buff, current_time: Time) -> BuffKey {
        let kind = buff.kind;
        // Try to find another overlaping non-queueable buff with same data, cat_ids and
        // source.
        let other_key = if kind.queues() {
            None
        } else {
            self.kinds[kind].as_ref().and_then(|(keys, _)| {
                keys.iter()
                    .find(|key| {
                        self.buffs.get(**key).is_some_and(|other_buff| {
                            other_buff.data == buff.data
                                && other_buff.cat_ids == buff.cat_ids
                                && other_buff.source == buff.source
                                && other_buff
                                    .end_time
                                    .is_none_or(|end_time| end_time.0 >= buff.start_time.0)
                        })
                    })
                    .copied()
            })
        };

        // If another buff with the same fields is found, update end_time and effects
        let key = if !kind.stacks()
            && let Some((other_buff, key)) =
                other_key.and_then(|key| Some((self.buffs.get_mut(key)?, key)))
        {
            other_buff.end_time = buff.end_time;
            other_buff.effects = buff.effects;
            key
        // Otherwise, insert a new buff
        } else {
            let key = self.buffs.insert(buff);
            self.kinds[kind]
                .get_or_insert_with(|| (Vec::new(), current_time))
                .0
                .push(key);
            key
        };

        self.sort_kind(kind);
        if kind.queues() {
            self.delay_queueable_buffs(kind, current_time);
        }
        key
    }

    pub fn contains(&self, kind: BuffKind) -> bool { self.kinds[kind].is_some() }

    pub fn contains_any(&self, kinds: &[BuffKind]) -> bool {
        kinds.iter().any(|kind| self.contains(*kind))
    }

    // Iterate through buffs of a given kind in effect order (most powerful first)
    pub fn iter_kind(&self, kind: BuffKind) -> impl Iterator<Item = (BuffKey, &Buff)> + '_ {
        self.kinds[kind]
            .as_ref()
            .map(|keys| keys.0.iter())
            .unwrap_or_else(|| [].iter())
            .map(move |&key| (key, &self.buffs[key]))
    }

    /// Whether `by` is a charmer or dominator of this entity — `Dominated`
    /// implies `Charmed`'s restrictions on top of command eligibility, so
    /// both kinds are checked. No-alloc `iter_kind` scan, modelled on
    /// `is_charmed_by` (`server/agent/src/action_nodes.rs`).
    pub fn charmed_by(&self, by: Uid) -> bool {
        [BuffKind::Charmed, BuffKind::Dominated].into_iter().any(|kind| {
            self.iter_kind(kind)
                .any(|(_, buff)| matches!(buff.source, BuffSource::Character { by: b, .. } if b == by))
        })
    }

    // Iterates through all active buffs (the most powerful buff of each
    // non-stacking kind, and all of the stacking ones)
    pub fn iter_active(&self) -> impl Iterator<Item = impl Iterator<Item = &Buff>> + '_ {
        self.kinds
            .iter()
            .filter_map(|(kind, keys)| keys.as_ref().map(|keys| (kind, keys)))
            .map(move |(kind, keys)| {
                if kind.stacks() {
                    // Iterate stackable buffs in reverse order to show the timer of the soonest one
                    // to expire
                    Either::Left(keys.0.iter().filter_map(|key| self.buffs.get(*key)).rev())
                } else {
                    Either::Right(self.buffs.get(keys.0[0]).into_iter())
                }
            })
    }

    // Gets most powerful buff of a given kind
    pub fn remove(&mut self, buff_key: BuffKey) {
        if let Some(buff) = self.buffs.remove(buff_key) {
            let kind = buff.kind;
            self.kinds[kind]
                .as_mut()
                .map(|keys| keys.0.retain(|key| *key != buff_key));
            self.sort_kind(kind);
        }
    }

    fn delay_queueable_buffs(&mut self, kind: BuffKind, current_time: Time) {
        let mut next_start_time: Option<Time> = None;
        debug_assert!(kind.queues());
        if let Some(buffs) = self.kinds[kind].as_mut() {
            buffs.0.iter().for_each(|key| {
                if let Some(buff) = self.buffs.get_mut(*key) {
                    // End time only being updated when there is some next_start_time will
                    // technically cause buffs to "end early" if they have a weaker strength than a
                    // buff with an infinite duration, but this is fine since those buffs wouldn't
                    // matter anyways
                    if let Some(next_start_time) = next_start_time {
                        // Delays buff so that it has the same progress it has now at the time the
                        // previous buff would end.
                        //
                        // Shift should be relative to current time, unless the buff is delayed and
                        // hasn't started yet
                        let reference_time = current_time.0.max(buff.start_time.0);
                        // If buff has a delay, ensure that queueables shuffling queue does not
                        // potentially allow skipping delay
                        buff.start_time = Time(next_start_time.0.max(buff.start_time.0));
                        buff.end_time = buff.end_time.map(|end| {
                            Time(end.0 + next_start_time.0.max(reference_time) - reference_time)
                        });
                    }
                    next_start_time = buff.end_time;
                }
            })
        }
    }

    pub fn remove_by_category(
        &mut self,
        all_required: Vec<BuffCategory>,
        any_required: Vec<BuffCategory>,
        none_required: Vec<BuffCategory>,
    ) {
        let mut keys_to_remove = Vec::new();
        for (key, buff) in self.buffs.iter() {
            let mut required_met = true;
            for required in &all_required {
                if !buff.cat_ids.iter().any(|cat| cat == required) {
                    required_met = false;
                    break;
                }
            }
            let mut any_met = any_required.is_empty();
            for any in &any_required {
                if buff.cat_ids.iter().any(|cat| cat == any) {
                    any_met = true;
                    break;
                }
            }
            let mut none_met = true;
            for none in &none_required {
                if buff.cat_ids.iter().any(|cat| cat == none) {
                    none_met = false;
                    break;
                }
            }
            if required_met && any_met && none_met {
                keys_to_remove.push(key);
            }
        }
        for key in keys_to_remove {
            self.remove(key);
        }
    }
}

impl Component for Buffs {
    type Storage = DerefFlaggedStorage<Self, VecStorage<Self>>;
}

#[derive(Default, Copy, Clone)]
pub struct DestInfo<'a> {
    pub stats: Option<&'a Stats>,
    pub mass: Option<&'a Mass>,
}

#[cfg(test)]
pub mod tests {
    use crate::comp::buff::*;

    #[cfg(test)]
    fn create_test_queueable_buff(buff_data: BuffData, time: Time) -> Buff {
        // Change to another buff that queues if we ever add one and remove saturation,
        // otherwise maybe add a test buff kind?
        debug_assert!(BuffKind::Saturation.queues());
        Buff::new(
            BuffKind::Saturation,
            buff_data,
            Vec::new(),
            BuffSource::Unknown,
            time,
            DestInfo::default(),
            None,
            None,
            None,
        )
    }

    #[test]
    fn difficult_terrain_halves_speed_at_half_strength() {
        // BL-03: linear slow, strength 0.5 -> exactly half move speed.
        let effects = BuffKind::DifficultTerrain.effects(&BuffData::new(0.5, None), None, None);
        assert!(effects.iter().any(|e| matches!(
            e,
            BuffEffect::MovementSpeed(s) if (*s - 0.5).abs() < f32::EPSILON
        )));
        assert!(!BuffKind::DifficultTerrain.is_buff(), "should be a debuff");
    }

    #[test]
    fn antimagic_disables_magic() {
        // BL-36: the antimagic debuff sets the disable-magic flag and nothing else.
        let effects = BuffKind::Antimagic.effects(&BuffData::new(1.0, None), None, None);
        assert_eq!(effects.len(), 1);
        assert!(matches!(effects[0], BuffEffect::DisableMagic));
        assert!(!BuffKind::Antimagic.is_buff(), "should be a debuff");
    }

    #[test]
    fn anchored_disables_teleport_only() {
        // BL-05 rider: anchor sets the disable-teleport flag and nothing else.
        let effects = BuffKind::Anchored.effects(&BuffData::new(1.0, None), None, None);
        assert_eq!(effects.len(), 1);
        assert!(matches!(effects[0], BuffEffect::DisableTeleport));
        assert!(!BuffKind::Anchored.is_buff(), "should be a debuff");
    }

    #[test]
    fn asleep_incapacitates() {
        // BL-05 rider: sleep roots (MovementSpeed 0) + locks auxiliary abilities.
        let effects = BuffKind::Asleep.effects(&BuffData::new(1.0, None), None, None);
        assert!(
            effects
                .iter()
                .any(|e| matches!(e, BuffEffect::MovementSpeed(s) if *s == 0.0))
        );
        assert!(
            effects
                .iter()
                .any(|e| matches!(e, BuffEffect::DisableAuxiliaryAbilities))
        );
        // Can't deal damage while asleep (no "disable all abilities" primitive).
        assert!(
            effects
                .iter()
                .any(|e| matches!(e, BuffEffect::AttackDamage(d) if *d == 0.0))
        );
        assert!(!BuffKind::Asleep.is_buff(), "should be a debuff");
    }

    #[test]
    fn agonized_debilitates_without_fully_incapacitating() {
        // Unlike Asleep/Paralyzed, movement and damage are reduced (strength
        // 0.5 -> halved), not zeroed, but the ability lock is the same.
        let effects = BuffKind::Agonized.effects(&BuffData::new(0.5, None), None, None);
        assert!(
            effects
                .iter()
                .any(|e| matches!(e, BuffEffect::MovementSpeed(s) if (*s - 0.5).abs() < 0.001))
        );
        assert!(
            effects
                .iter()
                .any(|e| matches!(e, BuffEffect::AttackDamage(d) if (*d - 0.5).abs() < 0.001))
        );
        assert!(
            effects
                .iter()
                .any(|e| matches!(e, BuffEffect::DisableAuxiliaryAbilities))
        );
        assert!(!BuffKind::Agonized.is_buff(), "should be a debuff");
    }

    #[test]
    fn restful_sleep_incapacitates_like_asleep_but_is_positive() {
        // Same incapacitation shape as Asleep...
        let asleep_effects = BuffKind::Asleep.effects(&BuffData::new(1.0, None), None, None);
        let restful_effects = BuffKind::RestfulSleep.effects(&BuffData::new(1.0, None), None, None);
        assert_eq!(asleep_effects.len(), restful_effects.len());
        assert!(
            restful_effects
                .iter()
                .any(|e| matches!(e, BuffEffect::MovementSpeed(s) if *s == 0.0))
        );
        assert!(
            restful_effects
                .iter()
                .any(|e| matches!(e, BuffEffect::DisableAuxiliaryAbilities))
        );
        // ...but classified as a buff, not a debuff, since it is only ever
        // granted to consenting allies.
        assert!(
            BuffKind::RestfulSleep.is_buff(),
            "should be a buff, unlike Asleep"
        );
    }

    #[test]
    fn blinded_reduces_attack_damage() {
        // BL-05 rider: blind reduces outgoing damage by strength (0.5 -> half).
        let effects = BuffKind::Blinded.effects(&BuffData::new(0.5, None), None, None);
        assert_eq!(effects.len(), 1);
        assert!(matches!(effects[0], BuffEffect::AttackDamage(d) if (d - 0.5).abs() < 1e-6));
        assert!(!BuffKind::Blinded.is_buff(), "should be a debuff");
    }

    #[test]
    fn is_heal_flags_healing_kinds_only() {
        // BL-66 d: heal_power should scale exactly the positive
        // HealthChangeOverTime kinds, not arbitrary buffs/debuffs.
        assert!(BuffKind::Regeneration.is_heal());
        assert!(!BuffKind::Slowed.is_heal());
    }

    #[test]
    fn slowed_reduces_movement_speed_only() {
        // BL-66 d: Slowed mirrors Crippled's movement curve without the HP
        // drain, and is a debuff.
        assert!(!BuffKind::Slowed.is_buff(), "should be a debuff");
        let effects = BuffKind::Slowed.effects(&BuffData::new(0.5, None), None, None);
        assert!(
            effects
                .iter()
                .any(|e| matches!(e, BuffEffect::MovementSpeed(s) if *s < 1.0))
        );
    }

    #[test]
    fn remote_sensing_roots_the_caster_even_while_piloting() {
        // A caster piloting an `arcane_eye` still has their own
        // ControllerInputs read by `pilot::Sys` and forwarded to the eye
        // (common/systems/src/pilot.rs) -- the caster's own body must stay
        // rooted (MovementSpeed 0.0) regardless, exactly as it is for every
        // other remote-sensing anchor kind. No damage immunity, no aggro
        // suppression accompanies this -- the body stays a fully normal,
        // hittable target; only movement is locked.
        let data = BuffData {
            misc_data: Some(MiscBuffData::RemoteSense {
                anchor_kind: SenseAnchorKind::Piloted,
                free_look: false,
                piloted: true,
                spawn_range: default_piloted_spawn_range(),
                flight_speed: default_piloted_flight_speed(),
                behind_dist: default_tracking_behind_dist(),
                above_dist: default_tracking_above_dist(),
            }),
            ..BuffData::new(0.0, Some(Secs(60.0)))
        };
        let effects = BuffKind::RemoteSensing.effects(&data, None, None);

        assert!(
            effects
                .iter()
                .any(|e| matches!(e, BuffEffect::MovementSpeed(s) if *s == 0.0)),
            "the caster's body must be fully rooted while remote-sensing, piloted or not"
        );
        assert!(
            effects.iter().any(|e| matches!(e, BuffEffect::RemoteSense {
                anchor_kind: SenseAnchorKind::Piloted,
                free_look: false,
                piloted: true,
            })),
            "the anchor shape must reach the effect unchanged"
        );
    }

    #[test]
    fn terrified_slows_and_lowers_accuracy() {
        // BL-05 Fear rider on the BL-52 engine: slows AND lowers accuracy
        // (fights at a disadvantage — more misses, same damage); flee behaviour
        // is in the agent AI.
        let effects = BuffKind::Terrified.effects(&BuffData::new(1.0, None), None, None);
        assert!(
            effects
                .iter()
                .any(|e| matches!(e, BuffEffect::MovementSpeed(s) if *s < 1.0))
        );
        assert!(
            effects
                .iter()
                .any(|e| matches!(e, BuffEffect::Accuracy(a) if *a < 0.0))
        );
    }

    #[test]
    fn freedom_of_movement_grants_difficult_terrain_immunity() {
        // BL-03: the immunity source negates DifficultTerrain and nothing else.
        let effects = BuffKind::FreedomOfMovement.effects(&BuffData::new(1.0, None), None, None);
        assert_eq!(effects.len(), 1);
        assert!(matches!(
            effects[0],
            BuffEffect::BuffImmunity(BuffKind::DifficultTerrain)
        ));
        assert!(BuffKind::FreedomOfMovement.is_buff(), "should be positive");
    }

    #[test]
    fn bleeding_mark_bleeds_and_is_a_debuff() {
        // BL-05 RD-7: bleeds over time (DoT) like Bleeding; the on-expire
        // detonation burst is emitted by the buff system, not as an effect here.
        let effects = BuffKind::BleedingMark.effects(&BuffData::new(5.0, None), None, None);
        assert!(matches!(
            effects.as_slice(),
            [BuffEffect::HealthChangeOverTime { rate, .. }] if *rate < 0.0
        ));
        assert!(!BuffKind::BleedingMark.is_buff(), "should be a debuff");
    }

    #[test]
    fn shielded_is_a_positive_buff_with_no_tick_effect() {
        // BL-05 RD-6: the absorb pool is granted on buff-add (server) and
        // consumed in `Health::change_by`, so the buff itself has no per-tick
        // effect; it's a positive marker buff.
        let effects = BuffKind::Shielded.effects(&BuffData::new(30.0, None), None, None);
        assert!(effects.is_empty());
        assert!(BuffKind::Shielded.is_buff(), "should be positive");
    }

    #[test]
    /// Tests a number of buffs with various progresses that queue to ensure
    /// queue has correct total duration
    fn test_queueable_buffs_three() {
        let mut buff_comp: Buffs = Default::default();
        let buff_data = BuffData::new(1.0, Some(Secs(10.0)));
        let time_a = Time(0.0);
        buff_comp.insert(create_test_queueable_buff(buff_data, time_a), time_a);
        let time_b = Time(6.0);
        buff_comp.insert(create_test_queueable_buff(buff_data, time_b), time_b);
        let time_c = Time(11.0);
        buff_comp.insert(create_test_queueable_buff(buff_data, time_c), time_c);
        // Check that all buffs have an end_time less than or equal to 30, and that at
        // least one has an end_time greater than or equal to 30.
        //
        // This should be true because 3 buffs that each lasted for 10 seconds were
        // inserted at various times, so the total duration should be 30 seconds.
        assert!(
            buff_comp
                .buffs
                .values()
                .all(|b| b.end_time.unwrap().0 < 30.01)
        );
        assert!(
            buff_comp
                .buffs
                .values()
                .any(|b| b.end_time.unwrap().0 > 29.99)
        );
    }

    #[test]
    /// Tests that if a buff had a delay but will start soon, and an immediate
    /// queueable buff is added, delayed buff has correct start time
    fn test_queueable_buff_delay_start() {
        let mut buff_comp: Buffs = Default::default();
        let queued_buff_data = BuffData::new(1.0, Some(Secs(10.0))).with_delay(Secs(10.0));
        let buff_data = BuffData::new(1.0, Some(Secs(10.0)));
        let time_a = Time(0.0);
        buff_comp.insert(create_test_queueable_buff(queued_buff_data, time_a), time_a);
        let time_b = Time(6.0);
        buff_comp.insert(create_test_queueable_buff(buff_data, time_b), time_b);
        // Check that all buffs have an end_time less than or equal to 26, and that at
        // least one has an end_time greater than or equal to 26.
        //
        // This should be true because the first buff added had a delay of 10 seconds
        // and a duration of 10 seconds, the second buff added at 6 seconds had no
        // delay, and a duration of 10 seconds. When it finishes at 16 seconds the first
        // buff is past the delay time so should finish at 26 seconds.
        assert!(
            buff_comp
                .buffs
                .values()
                .all(|b| b.end_time.unwrap().0 < 26.01)
        );
        assert!(
            buff_comp
                .buffs
                .values()
                .any(|b| b.end_time.unwrap().0 > 25.99)
        );
    }

    #[test]
    /// Tests that if a buff had a long delay, a short immediate queueable buff
    /// does not move delayed buff start or end times
    fn test_queueable_buff_long_delay() {
        let mut buff_comp: Buffs = Default::default();
        let queued_buff_data = BuffData::new(1.0, Some(Secs(10.0))).with_delay(Secs(50.0));
        let buff_data = BuffData::new(1.0, Some(Secs(10.0)));
        let time_a = Time(0.0);
        buff_comp.insert(create_test_queueable_buff(queued_buff_data, time_a), time_a);
        let time_b = Time(10.0);
        buff_comp.insert(create_test_queueable_buff(buff_data, time_b), time_b);
        // Check that all buffs have either an end time less than or equal to 20 seconds
        // XOR a start time greater than or equal to 50 seconds, that all buffs have a
        // start time less than or equal to 50 seconds, that all buffs have an end time
        // less than or equal to 60 seconds, and that at least one buff has an end time
        // greater than or equal to 60 seconds
        //
        // This should be true because the first buff has a delay of 50 seconds, the
        // second buff added has no delay at 10 seconds and lasts 10 seconds, so should
        // end at 20 seconds and not affect the start time of the delayed buff, and
        // since the delayed buff was not affected the end time should be 10 seconds
        // after the start time: 60 seconds != used here to emulate xor
        assert!(
            buff_comp
                .buffs
                .values()
                .all(|b| (b.end_time.unwrap().0 < 20.01) != (b.start_time.0 > 49.99))
        );
        assert!(buff_comp.buffs.values().all(|b| b.start_time.0 < 50.01));
        assert!(
            buff_comp
                .buffs
                .values()
                .all(|b| b.end_time.unwrap().0 < 60.01)
        );
        assert!(
            buff_comp
                .buffs
                .values()
                .any(|b| b.end_time.unwrap().0 > 59.99)
        );
    }

    // ENG-C2 (M5): concentration breaks when a single hit deals at least the
    // (max-HP-scaled) damage threshold (Matias §6.5 "umbral de daño").
    #[test]
    fn concentration_breaks_at_threshold() {
        let max_hp = 100.0;
        let t = concentration_break_threshold(max_hp);
        assert!(!concentration_breaks(t - 0.01, max_hp));
        assert!(concentration_breaks(t, max_hp));
        assert!(concentration_breaks(t + 50.0, max_hp));
        assert!(!concentration_breaks(0.0, max_hp));
    }

    // ENG-C2 refinement (Matias): a more powerful (higher-max-HP) bearer resists
    // more — the same hit that breaks a weak caster may not break a strong one.
    #[test]
    fn concentration_threshold_scales_with_power() {
        let weak = concentration_break_threshold(50.0);
        let strong = concentration_break_threshold(800.0);
        assert!(strong > weak);
        let hit = weak + 1.0;
        assert!(concentration_breaks(hit, 50.0)); // breaks the weak caster
        assert!(!concentration_breaks(hit, 800.0)); // same hit spares the strong one
    }

    // Concentration is a buff category so concentration-sustained effects can be
    // tagged in RON and removed together (one-at-a-time + break-on-damage).
    #[test]
    fn concentration_is_a_buff_category() {
        let cat: BuffCategory = ron::from_str("Concentration").expect("Concentration must parse");
        assert_eq!(cat, BuffCategory::Concentration);
    }

    #[test]
    fn pass_without_trace_grants_only_stealth() {
        let effects = BuffKind::PassWithoutTrace.effects(&BuffData::new(1.0, None), None, None);
        assert_eq!(effects.len(), 1);
        assert!(matches!(effects[0], BuffEffect::Stealth(s) if (s - 1.0).abs() < 1e-6));
        assert!(BuffKind::PassWithoutTrace.is_buff(), "should be a buff");
    }

    #[test]
    fn mooncloak_grants_stealth_and_an_authored_magic_resistance() {
        let data = BuffData::new(1.5, None).with_misc_data(MiscBuffData::ResistMagic(0.10));
        let effects = BuffKind::Mooncloak.effects(&data, None, None);
        assert_eq!(effects.len(), 2);
        assert!(
            effects
                .iter()
                .any(|e| matches!(e, BuffEffect::Stealth(s) if (*s - 1.5).abs() < 1e-6))
        );
        assert!(effects.iter().any(|e| matches!(
            e,
            BuffEffect::Resistance(ResistKind::Magic, r) if (*r - 0.10).abs() < 1e-6
        )));
        assert!(BuffKind::Mooncloak.is_buff(), "should be a buff");
    }

    #[test]
    fn mooncloak_grants_only_stealth_when_misc_data_is_absent() {
        // Regression guard: without an authored ResistMagic, the buff must
        // not silently fall back to a derived value -- it should just omit
        // the resistance effect entirely.
        let effects = BuffKind::Mooncloak.effects(&BuffData::new(1.5, None), None, None);
        assert_eq!(effects.len(), 1);
        assert!(matches!(effects[0], BuffEffect::Stealth(s) if (s - 1.5).abs() < 1e-6));
    }

    #[test]
    fn nondetection_grants_only_the_flag() {
        let effects = BuffKind::Nondetection.effects(&BuffData::new(0.0, None), None, None);
        assert_eq!(effects.len(), 1);
        assert!(matches!(effects[0], BuffEffect::Nondetection));
        assert!(BuffKind::Nondetection.is_buff(), "should be a buff");
    }

    #[test]
    fn magic_aura_grants_the_authored_false_aura() {
        let data =
            BuffData::new(0.0, None).with_misc_data(MiscBuffData::FalseAura(SenseKind::Magic));
        let effects = BuffKind::MagicAura.effects(&data, None, None);
        assert_eq!(effects.len(), 1);
        assert!(matches!(
            effects[0],
            BuffEffect::FalseAura(SenseKind::Magic)
        ));
        assert!(BuffKind::MagicAura.is_buff(), "should be a buff");
    }

    #[test]
    fn magic_aura_grants_nothing_when_misc_data_is_absent() {
        // Regression guard: without an authored SenseKind, the buff must not
        // silently fall back to a default -- it should just omit the effect
        // entirely, same as Mooncloak's own ResistMagic guard above.
        let effects = BuffKind::MagicAura.effects(&BuffData::new(0.0, None), None, None);
        assert!(effects.is_empty());
    }

    #[test]
    fn sequester_grants_nondetection_and_a_large_stealth_contribution() {
        let effects = BuffKind::Sequester.effects(&BuffData::new(20.0, None), None, None);
        assert_eq!(effects.len(), 2);
        assert!(
            effects
                .iter()
                .any(|e| matches!(e, BuffEffect::Nondetection))
        );
        assert!(
            effects
                .iter()
                .any(|e| matches!(e, BuffEffect::Stealth(s) if (*s - 20.0).abs() < 1e-6))
        );
        assert!(BuffKind::Sequester.is_buff(), "should be a buff");
    }

    #[test]
    fn see_invisible_grants_only_pierce_concealment_and_never_a_sense() {
        // Regression guard for the bug this row fixes: SeeInvisible must
        // never emit a `Sense` effect, even if a caller mistakenly supplies
        // `misc_data` -- its entire job is the concealment-piercing flag.
        let data = BuffData::new(0.0, None).with_misc_data(MiscBuffData::Sense(
            SenseKind::Magic,
            20.0,
            SenseMode::Snapshot,
        ));
        let effects = BuffKind::SeeInvisible.effects(&data, None, None);
        assert_eq!(effects.len(), 1);
        assert!(matches!(effects[0], BuffEffect::PierceConcealment));
        assert!(
            !effects
                .iter()
                .any(|e| matches!(e, BuffEffect::Sense { .. })),
            "SeeInvisible must never produce a reveal-set Sense effect"
        );
        assert!(BuffKind::SeeInvisible.is_buff(), "should be a buff");
    }

    #[test]
    fn true_sight_grants_all_four_effects_when_misc_data_is_present() {
        let data = BuffData::new(0.0, None).with_misc_data(MiscBuffData::Sense(
            SenseKind::True,
            30.0,
            SenseMode::Continuous,
        ));
        let effects = BuffKind::TrueSight.effects(&data, None, None);
        assert_eq!(effects.len(), 4);
        assert!(effects.iter().any(|e| matches!(
            e,
            BuffEffect::Sense {
                kind: SenseKind::True,
                radius,
                mode: SenseMode::Continuous,
            } if (*radius - 30.0).abs() < 1e-6
        )));
        assert!(
            effects
                .iter()
                .any(|e| matches!(e, BuffEffect::PierceConcealment))
        );
        assert!(
            effects
                .iter()
                .any(|e| matches!(e, BuffEffect::PierceIllusion))
        );
        assert!(
            effects
                .iter()
                .any(|e| matches!(e, BuffEffect::PierceDarkness))
        );
        assert!(BuffKind::TrueSight.is_buff(), "should be a buff");
    }

    #[test]
    fn true_sight_grants_only_the_three_pierce_flags_when_misc_data_is_absent() {
        // Same "omit rather than guess" pattern as Mooncloak's own regression
        // guard: without an authored Sense, the buff must not invent one --
        // it should just omit the reveal-set effect, keeping only the
        // unconditional pierce flags.
        let effects = BuffKind::TrueSight.effects(&BuffData::new(0.0, None), None, None);
        assert_eq!(effects.len(), 3);
        assert!(
            !effects
                .iter()
                .any(|e| matches!(e, BuffEffect::Sense { .. })),
            "no misc_data means no reveal-set Sense effect"
        );
        assert!(
            effects
                .iter()
                .any(|e| matches!(e, BuffEffect::PierceConcealment))
        );
        assert!(
            effects
                .iter()
                .any(|e| matches!(e, BuffEffect::PierceIllusion))
        );
        assert!(
            effects
                .iter()
                .any(|e| matches!(e, BuffEffect::PierceDarkness))
        );
    }
}
