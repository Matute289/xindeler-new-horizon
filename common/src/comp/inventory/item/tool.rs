// Note: If you changes here "break" old character saves you can change the
// version in voxygen\src\meta.rs in order to reset save files to being empty

use crate::{
    assets::{Asset, AssetCache, AssetExt, AssetHandle, BoxedError, Ron, SharedString},
    comp::{
        CharacterAbility, Combo, SkillSet,
        ability::Stance,
        buff::{BuffKind, Buffs},
        inventory::{
            Inventory,
            item::{DurabilityMultiplier, ItemKind},
            slot::EquipSlot,
        },
        skills::Skill,
    },
};
use hashbrown::HashMap;
use serde::{Deserialize, Serialize};
use std::ops::{Add, AddAssign, Div, Mul, MulAssign, Sub};
use strum::EnumIter;
use tracing::warn;

#[derive(
    Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, Ord, PartialOrd, EnumIter,
)]
pub enum ToolKind {
    // weapons
    Sword,
    Axe,
    Hammer,
    Bow,
    Staff,
    Sceptre,
    // caster implements (magic-system-v2 spec §2)
    Tome,
    HolySymbol,
    Focus,
    // future weapons
    Dagger,
    Shield,
    Spear,
    Blowgun,
    // tools
    Debug,
    Farming,
    Pick,
    Shovel,
    /// Music Instruments
    Instrument,
    /// Throwable item
    Throwable,
    // npcs
    /// Intended for invisible weapons (e.g. a creature using its claws or
    /// biting)
    Natural,
    /// This is an placeholder item, it is used by non-humanoid npcs to attack
    Empty,
}

/// A second within-`ToolKind` discriminator, orthogonal to [`Hands`],
/// distinguishing a caster implement's magic use from a martial/melee use of
/// the *same* `ToolKind`. Introduced so `Staff`/`Sceptre` can carry two
/// distinct kits (a Mage's caster staff vs a Monk's martial quarterstaff)
/// without adding new `ToolKind` variants (`ToolKind` is upstream-owned).
/// `Tool.role` is `Option<WeaponRole>` — `None` means "whatever
/// `ToolKind::default_role` says" so the hundreds of shipped `kind: Tool((`
/// RONs never need editing; only a deviation (e.g. a martial staff) declares
/// an explicit `role:`.
// `Ord`/`PartialOrd` are needed so `WeaponRole` can compose into a
// `SkillGroupKind` variant's derive, the same way `ToolKind` already does for
// `Weapon(ToolKind)`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Ord, PartialOrd, Serialize, Deserialize)]
pub enum WeaponRole {
    Martial,
    Caster,
}

impl ToolKind {
    /// The role a `Tool` of this kind has when it declares no explicit
    /// `role:` in its RON. Exhaustive (no `_` arm) so a future `ToolKind`
    /// variant forces a deliberate choice here instead of silently defaulting.
    pub fn default_role(self) -> WeaponRole {
        match self {
            ToolKind::Staff
            | ToolKind::Sceptre
            | ToolKind::Tome
            | ToolKind::HolySymbol
            | ToolKind::Focus => WeaponRole::Caster,
            ToolKind::Sword
            | ToolKind::Axe
            | ToolKind::Hammer
            | ToolKind::Bow
            | ToolKind::Dagger
            | ToolKind::Shield
            | ToolKind::Spear
            | ToolKind::Blowgun
            | ToolKind::Debug
            | ToolKind::Farming
            | ToolKind::Pick
            | ToolKind::Shovel
            | ToolKind::Instrument
            | ToolKind::Throwable
            | ToolKind::Natural
            | ToolKind::Empty => WeaponRole::Martial,
        }
    }

    pub fn identifier_name(&self) -> &'static str {
        match self {
            ToolKind::Sword => "sword",
            ToolKind::Axe => "axe",
            ToolKind::Hammer => "hammer",
            ToolKind::Bow => "bow",
            ToolKind::Dagger => "dagger",
            ToolKind::Staff => "staff",
            ToolKind::Spear => "spear",
            ToolKind::Blowgun => "blowgun",
            ToolKind::Sceptre => "sceptre",
            ToolKind::Tome => "tome",
            ToolKind::HolySymbol => "holy_symbol",
            ToolKind::Focus => "focus",
            ToolKind::Shield => "shield",
            ToolKind::Natural => "natural",
            ToolKind::Debug => "debug",
            ToolKind::Farming => "farming",
            ToolKind::Pick => "pickaxe",
            ToolKind::Shovel => "shovel",
            ToolKind::Instrument => "instrument",
            ToolKind::Throwable => "throwable",
            ToolKind::Empty => "empty",
        }
    }

    pub fn gains_combat_xp(&self) -> bool {
        matches!(
            self,
            ToolKind::Sword
                | ToolKind::Axe
                | ToolKind::Hammer
                | ToolKind::Bow
                | ToolKind::Dagger
                | ToolKind::Staff
                | ToolKind::Spear
                | ToolKind::Blowgun
                | ToolKind::Sceptre
                | ToolKind::Shield
        )
    }

    pub fn can_block(&self) -> bool {
        matches!(
            self,
            ToolKind::Sword
                | ToolKind::Axe
                | ToolKind::Hammer
                | ToolKind::Shield
                | ToolKind::Dagger
        )
    }

    pub fn block_priority(&self) -> i32 {
        match self {
            ToolKind::Debug => 0,
            ToolKind::Blowgun => 1,
            ToolKind::Bow => 2,
            ToolKind::Staff => 3,
            ToolKind::Sceptre => 4,
            ToolKind::Tome => 3,
            ToolKind::HolySymbol => 4,
            ToolKind::Focus => 3,
            ToolKind::Empty => 5,
            ToolKind::Natural => 6,
            ToolKind::Throwable => 7,
            ToolKind::Instrument => 8,
            ToolKind::Farming => 9,
            ToolKind::Shovel => 10,
            ToolKind::Pick => 11,
            ToolKind::Dagger => 12,
            ToolKind::Spear => 13,
            ToolKind::Hammer => 14,
            ToolKind::Axe => 15,
            ToolKind::Sword => 16,
            ToolKind::Shield => 17,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Hands {
    One,
    Two,
}

bitflags::bitflags! {
    /// Per-`ToolKind` weapon-proficiency bitset. `Sword` is split into two
    /// bits (`SWORD_1H`/`SWORD_2H`) instead of getting one bit like every
    /// other variant, because `ToolKind::Sword` covers both `sword/` (2h
    /// greatswords) and `sword_1h/` (1h gladii) assets — the grip lives on
    /// the item's `Hands` field, not the tool kind. `Staff`/`Sceptre` are
    /// likewise split into `_CASTER`/`_MARTIAL` pairs keyed on [`WeaponRole`]
    /// instead of a single bit, mirroring the `Hands` split exactly. Build/
    /// query via [`ToolKindMask::for_tool`]/[`ToolKindMask::allows`], never by
    /// constructing bits directly. `Default` is deliberately empty
    /// (non-permissive) — permissiveness is an explicit opt-in via
    /// `ToolKindMask::all()` at the call site, so a missing narrowing can
    /// never silently read as "proficient with everything".
    #[derive(Copy, Clone, Debug, PartialEq, Eq, Default, Serialize, Deserialize)]
    pub struct ToolKindMask: u32 {
        const SWORD_1H      = 1 << 0;
        const SWORD_2H      = 1 << 1;
        const AXE           = 1 << 2;
        const HAMMER        = 1 << 3;
        const BOW           = 1 << 4;
        const STAFF_CASTER  = 1 << 5;
        const STAFF_MARTIAL = 1 << 6;
        const SCEPTRE_CASTER  = 1 << 7;
        const SCEPTRE_MARTIAL = 1 << 8;
        const TOME        = 1 << 9;
        const HOLY_SYMBOL = 1 << 10;
        const FOCUS       = 1 << 11;
        const DAGGER      = 1 << 12;
        const SHIELD      = 1 << 13;
        const SPEAR       = 1 << 14;
        const BLOWGUN     = 1 << 15;
        const DEBUG       = 1 << 16;
        const FARMING     = 1 << 17;
        const PICK        = 1 << 18;
        const SHOVEL      = 1 << 19;
        const INSTRUMENT  = 1 << 20;
        const THROWABLE   = 1 << 21;
        const NATURAL     = 1 << 22;
        const EMPTY       = 1 << 23;
    }
}

impl ToolKindMask {
    /// The bit(s) covering `kind` at grip `hands` and role `role`. `hands ==
    /// None` (unknown grip, e.g. a natural/NPC attack resolved with no item
    /// in hand) means "either grip" for `Sword`; `role == None` likewise
    /// means "either role" for `Staff`/`Sceptre`. Every other `ToolKind` has
    /// exactly one bit regardless of `hands`/`role`. Exhaustive match (no
    /// `_ =>` arm) so a future `ToolKind` variant added without a
    /// corresponding bit fails the build.
    pub fn for_tool(kind: ToolKind, hands: Option<Hands>, role: Option<WeaponRole>) -> Self {
        match kind {
            ToolKind::Sword => match hands {
                Some(Hands::One) => Self::SWORD_1H,
                Some(Hands::Two) => Self::SWORD_2H,
                None => Self::SWORD_1H | Self::SWORD_2H,
            },
            ToolKind::Staff => match role {
                Some(WeaponRole::Caster) => Self::STAFF_CASTER,
                Some(WeaponRole::Martial) => Self::STAFF_MARTIAL,
                None => Self::STAFF_CASTER | Self::STAFF_MARTIAL,
            },
            ToolKind::Sceptre => match role {
                Some(WeaponRole::Caster) => Self::SCEPTRE_CASTER,
                Some(WeaponRole::Martial) => Self::SCEPTRE_MARTIAL,
                None => Self::SCEPTRE_CASTER | Self::SCEPTRE_MARTIAL,
            },
            ToolKind::Axe => Self::AXE,
            ToolKind::Hammer => Self::HAMMER,
            ToolKind::Bow => Self::BOW,
            ToolKind::Tome => Self::TOME,
            ToolKind::HolySymbol => Self::HOLY_SYMBOL,
            ToolKind::Focus => Self::FOCUS,
            ToolKind::Dagger => Self::DAGGER,
            ToolKind::Shield => Self::SHIELD,
            ToolKind::Spear => Self::SPEAR,
            ToolKind::Blowgun => Self::BLOWGUN,
            ToolKind::Debug => Self::DEBUG,
            ToolKind::Farming => Self::FARMING,
            ToolKind::Pick => Self::PICK,
            ToolKind::Shovel => Self::SHOVEL,
            ToolKind::Instrument => Self::INSTRUMENT,
            ToolKind::Throwable => Self::THROWABLE,
            ToolKind::Natural => Self::NATURAL,
            ToolKind::Empty => Self::EMPTY,
        }
    }

    /// Is this mask proficient with `kind` at grip `hands` and role `role`?
    /// Uses `intersects` rather than `contains`: an unknown grip (`hands ==
    /// None`) resolves permissively for `Sword`, and an unknown role (`role
    /// == None`) resolves permissively for `Staff`/`Sceptre`, so having
    /// either bit of the relevant pair is enough, not both.
    pub fn allows(self, kind: ToolKind, hands: Option<Hands>, role: Option<WeaponRole>) -> bool {
        self.intersects(Self::for_tool(kind, hands, role))
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct Stats {
    pub equip_time_secs: f32,
    pub power: f32,
    pub effect_power: f32,
    pub speed: f32,
    pub range: f32,
    pub energy_efficiency: f32,
    pub buff_strength: f32,
}

impl Stats {
    pub fn zero() -> Stats {
        Stats {
            equip_time_secs: 0.0,
            power: 0.0,
            effect_power: 0.0,
            speed: 0.0,
            range: 0.0,
            energy_efficiency: 0.0,
            buff_strength: 0.0,
        }
    }

    pub fn one() -> Stats {
        Stats {
            equip_time_secs: 1.0,
            power: 1.0,
            effect_power: 1.0,
            speed: 1.0,
            range: 1.0,
            energy_efficiency: 1.0,
            buff_strength: 1.0,
        }
    }

    /// Calculates a diminished buff strength where the buff strength is clamped
    /// by the power, and then excess buff strength above the power is added
    /// with diminishing returns.
    // TODO: Remove this later when there are more varied high tier materials.
    // Mainly exists for now as a hack to allow some progression in strength of
    // directly applied buffs.
    pub fn diminished_buff_strength(&self) -> f32 {
        let base = self.buff_strength.clamp(0.0, self.power);
        let diminished = (self.buff_strength - base + 1.0).log(5.0);
        base + diminished
    }

    pub fn with_durability_mult(&self, dur_mult: DurabilityMultiplier) -> Self {
        let less_scaled = dur_mult.0 * 0.5 + 0.5;
        Self {
            equip_time_secs: self.equip_time_secs / less_scaled.max(0.01),
            power: self.power * dur_mult.0,
            effect_power: self.effect_power * dur_mult.0,
            speed: self.speed * less_scaled,
            range: self.range * less_scaled,
            energy_efficiency: self.energy_efficiency * less_scaled,
            buff_strength: self.buff_strength * dur_mult.0,
        }
    }
}

impl Add<Stats> for Stats {
    type Output = Self;

    fn add(self, other: Self) -> Self {
        Self {
            equip_time_secs: self.equip_time_secs + other.equip_time_secs,
            power: self.power + other.power,
            effect_power: self.effect_power + other.effect_power,
            speed: self.speed + other.speed,
            range: self.range + other.range,
            energy_efficiency: self.energy_efficiency + other.energy_efficiency,
            buff_strength: self.buff_strength + other.buff_strength,
        }
    }
}

impl AddAssign<Stats> for Stats {
    fn add_assign(&mut self, other: Stats) { *self = *self + other; }
}

impl Sub<Stats> for Stats {
    type Output = Self;

    fn sub(self, other: Self) -> Self::Output {
        Self {
            equip_time_secs: self.equip_time_secs - other.equip_time_secs,
            power: self.power - other.power,
            effect_power: self.effect_power - other.effect_power,
            speed: self.speed - other.speed,
            range: self.range - other.range,
            energy_efficiency: self.energy_efficiency - other.energy_efficiency,
            buff_strength: self.buff_strength - other.buff_strength,
        }
    }
}

impl Mul<Stats> for Stats {
    type Output = Self;

    fn mul(self, other: Self) -> Self {
        Self {
            equip_time_secs: self.equip_time_secs * other.equip_time_secs,
            power: self.power * other.power,
            effect_power: self.effect_power * other.effect_power,
            speed: self.speed * other.speed,
            range: self.range * other.range,
            energy_efficiency: self.energy_efficiency * other.energy_efficiency,
            buff_strength: self.buff_strength * other.buff_strength,
        }
    }
}

impl MulAssign<Stats> for Stats {
    fn mul_assign(&mut self, other: Stats) { *self = *self * other; }
}

impl Div<f32> for Stats {
    type Output = Self;

    fn div(self, scalar: f32) -> Self {
        Self {
            equip_time_secs: self.equip_time_secs / scalar,
            power: self.power / scalar,
            effect_power: self.effect_power / scalar,
            speed: self.speed / scalar,
            range: self.range / scalar,
            energy_efficiency: self.energy_efficiency / scalar,
            buff_strength: self.buff_strength / scalar,
        }
    }
}

impl Mul<DurabilityMultiplier> for Stats {
    type Output = Self;

    fn mul(self, value: DurabilityMultiplier) -> Self { self.with_durability_mult(value) }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Tool {
    pub kind: ToolKind,
    pub hands: Hands,
    /// `None` = "whatever `kind.default_role()` says". Only a deviation
    /// (e.g. a martial-role `Staff`) needs to declare this explicitly; read
    /// it via [`Tool::role`], never the raw field, so the default always
    /// applies. See [`WeaponRole`].
    #[serde(default)]
    role: Option<WeaponRole>,
    stats: Stats,
    // TODO: item specific abilities
}

impl Tool {
    // DO NOT USE UNLESS YOU KNOW WHAT YOU ARE DOING
    // Added for CSV import of stats
    pub fn new(kind: ToolKind, hands: Hands, role: Option<WeaponRole>, stats: Stats) -> Self {
        Self {
            kind,
            hands,
            role,
            stats,
        }
    }

    pub fn empty() -> Self {
        Self {
            kind: ToolKind::Empty,
            hands: Hands::One,
            role: None,
            stats: Stats {
                equip_time_secs: 0.0,
                power: 1.00,
                effect_power: 1.00,
                speed: 1.00,
                range: 1.0,
                energy_efficiency: 1.0,
                buff_strength: 1.0,
            },
        }
    }

    /// This tool's effective [`WeaponRole`]: the explicit `role:` if the RON
    /// declared one, otherwise `kind`'s default.
    pub fn role(&self) -> WeaponRole { self.role.unwrap_or(self.kind.default_role()) }

    pub fn stats(&self, durability_multiplier: DurabilityMultiplier) -> Stats {
        self.stats * durability_multiplier
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct AbilitySet<T> {
    pub guard: Option<AbilityKind<T>>,
    pub primary: AbilityKind<T>,
    pub secondary: AbilityKind<T>,
    pub abilities: Vec<AbilityKind<T>>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum AbilityKind<T> {
    Simple(Option<Skill>, T),
    Contextualized {
        pseudo_id: String,
        abilities: Vec<(AbilityContext, (Option<Skill>, T))>,
    },
}

/// The contextual index indicates which entry in a contextual ability was used.
/// This should only be necessary for the frontend to distinguish between the
/// options when a contextual ability is used.
#[derive(Clone, Debug, Serialize, Deserialize, Copy, Eq, PartialEq)]
pub struct ContextualIndex(pub usize);

impl<T> AbilityKind<T> {
    pub fn map<U, F: FnMut(T) -> U>(self, mut f: F) -> AbilityKind<U> {
        match self {
            Self::Simple(s, x) => AbilityKind::<U>::Simple(s, f(x)),
            Self::Contextualized {
                pseudo_id,
                abilities,
            } => AbilityKind::<U>::Contextualized {
                pseudo_id,
                abilities: abilities
                    .into_iter()
                    .map(|(c, (s, x))| (c, (s, f(x))))
                    .collect(),
            },
        }
    }

    pub fn map_ref<U, F: FnMut(&T) -> U>(&self, mut f: F) -> AbilityKind<U> {
        match self {
            Self::Simple(s, x) => AbilityKind::<U>::Simple(*s, f(x)),
            Self::Contextualized {
                pseudo_id,
                abilities,
            } => AbilityKind::<U>::Contextualized {
                pseudo_id: pseudo_id.clone(),
                abilities: abilities
                    .iter()
                    .map(|(c, (s, x))| (*c, (*s, f(x))))
                    .collect(),
            },
        }
    }

    pub fn ability(
        &self,
        skillset: Option<&SkillSet>,
        stance: Option<&Stance>,
        inv: Option<&Inventory>,
        combo: Option<&Combo>,
        buffs: Option<&Buffs>,
    ) -> Option<(&T, Option<ContextualIndex>)> {
        let unlocked = |s: Option<Skill>, a| {
            // If there is a skill requirement and the skillset does not contain the
            // required skill, return None
            s.is_none_or(|s| skillset.is_some_and(|ss| ss.has_skill(s)))
                .then_some(a)
        };

        match self {
            AbilityKind::Simple(s, a) => unlocked(*s, a).map(|a| (a, None)),
            AbilityKind::Contextualized {
                pseudo_id: _,
                abilities,
            } => abilities
                .iter()
                .enumerate()
                .filter_map(|(i, (req_contexts, (s, a)))| {
                    unlocked(*s, a).map(|a| (i, (req_contexts, a)))
                })
                .find_map(|(i, (req_context, a))| {
                    req_context
                        .fulfilled_by(stance, inv, combo, buffs)
                        .then_some((a, Some(ContextualIndex(i))))
                }),
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, Copy, Eq, PartialEq, Hash, Default)]
pub struct AbilityContext {
    /// Note, in this context `Stance::None` isn't intended to be used. e.g. the
    /// stance field should be `None` instead of `Some(Stance::None)` in the
    /// ability map config files(s).
    pub stance: Option<Stance>,
    #[serde(default)]
    pub dual_wielding_same_kind: bool,
    pub combo: Option<u32>,
    pub buff: Option<BuffKind>,
}

impl AbilityContext {
    fn fulfilled_by(
        &self,
        stance: Option<&Stance>,
        inv: Option<&Inventory>,
        combo: Option<&Combo>,
        buffs: Option<&Buffs>,
    ) -> bool {
        let dual_wielding_same_kind = if let Some(inv) = inv {
            let tool_kind = |slot| {
                inv.equipped(slot).and_then(|i| {
                    if let ItemKind::Tool(tool) = &*i.kind() {
                        Some(tool.kind)
                    } else {
                        None
                    }
                })
            };
            tool_kind(EquipSlot::ActiveMainhand) == tool_kind(EquipSlot::ActiveOffhand)
        } else {
            false
        };

        // Either stance not required or context is in the same stance
        let stance_check = self.stance.is_none_or(|s| stance.copied() == Some(s));
        // Either dual wield not required or context is dual wielding
        let dual_wield_check = !self.dual_wielding_same_kind || dual_wielding_same_kind;
        // Either no minimum combo needed or context has sufficient combo
        let combo_check = self
            .combo
            .is_none_or(|c_req| combo.is_some_and(|c| c.counter() >= c_req));
        // Either no buff requored or entity has buff present
        let buff_check = self
            .buff
            .is_none_or(|b| buffs.is_some_and(|buffs| buffs.contains(b)));

        stance_check && dual_wield_check && combo_check && buff_check
    }
}

impl AbilitySet<AbilityItem> {
    #[must_use]
    pub fn modified_by_tool(
        self,
        tool: &Tool,
        durability_multiplier: DurabilityMultiplier,
    ) -> Self {
        self.map(|a| AbilityItem {
            id: a.id,
            ability: a
                .ability
                .adjusted_by_stats(tool.stats(durability_multiplier)),
        })
    }
}

impl<T> AbilitySet<T> {
    pub fn map<U, F: FnMut(T) -> U>(self, mut f: F) -> AbilitySet<U> {
        AbilitySet {
            guard: self.guard.map(|g| g.map(&mut f)),
            primary: self.primary.map(&mut f),
            secondary: self.secondary.map(&mut f),
            abilities: self.abilities.into_iter().map(|x| x.map(&mut f)).collect(),
        }
    }

    pub fn map_ref<U, F: FnMut(&T) -> U>(&self, mut f: F) -> AbilitySet<U> {
        AbilitySet {
            guard: self.guard.as_ref().map(|g| g.map_ref(&mut f)),
            primary: self.primary.map_ref(&mut f),
            secondary: self.secondary.map_ref(&mut f),
            abilities: self.abilities.iter().map(|x| x.map_ref(&mut f)).collect(),
        }
    }

    pub fn guard(
        &self,
        skillset: Option<&SkillSet>,
        stance: Option<&Stance>,
        inv: Option<&Inventory>,
        combo: Option<&Combo>,
        buffs: Option<&Buffs>,
    ) -> Option<(&T, Option<ContextualIndex>)> {
        self.guard
            .as_ref()
            .and_then(|g| g.ability(skillset, stance, inv, combo, buffs))
    }

    pub fn primary(
        &self,
        skillset: Option<&SkillSet>,
        stance: Option<&Stance>,
        inv: Option<&Inventory>,
        combo: Option<&Combo>,
        buffs: Option<&Buffs>,
    ) -> Option<(&T, Option<ContextualIndex>)> {
        self.primary.ability(skillset, stance, inv, combo, buffs)
    }

    pub fn secondary(
        &self,
        skillset: Option<&SkillSet>,
        stance: Option<&Stance>,
        inv: Option<&Inventory>,
        combo: Option<&Combo>,
        buffs: Option<&Buffs>,
    ) -> Option<(&T, Option<ContextualIndex>)> {
        self.secondary.ability(skillset, stance, inv, combo, buffs)
    }

    pub fn auxiliary(
        &self,
        index: usize,
        skillset: Option<&SkillSet>,
        stance: Option<&Stance>,
        inv: Option<&Inventory>,
        combo: Option<&Combo>,
        buffs: Option<&Buffs>,
    ) -> Option<(&T, Option<ContextualIndex>)> {
        self.abilities
            .get(index)
            .and_then(|a| a.ability(skillset, stance, inv, combo, buffs))
    }
}

impl Default for AbilitySet<AbilityItem> {
    fn default() -> Self {
        AbilitySet {
            guard: None,
            primary: AbilityKind::Simple(None, AbilityItem {
                id: String::new(),
                ability: CharacterAbility::default(),
            }),
            secondary: AbilityKind::Simple(None, AbilityItem {
                id: String::new(),
                ability: CharacterAbility::default(),
            }),
            abilities: Vec::new(),
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq, Hash)]
pub enum AbilitySpec {
    Tool(ToolKind),
    Custom(String),
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct AbilityItem {
    pub id: String,
    pub ability: CharacterAbility,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum AbilityMapEntry<T = AbilityItem> {
    AbilitySet(AbilitySet<T>),
    AbilitySetOverride {
        parent: AbilitySpec,
        guard: Option<AbilityKind<T>>,
        primary: Option<AbilityKind<T>>,
        secondary: Option<AbilityKind<T>>,
        added_abilities: Vec<AbilityKind<T>>,
        removed_abilities: Vec<AbilityKind<T>>,
    },
}

impl<T: Clone + Eq> AbilityMapEntry<T> {
    pub fn map_ref<U, F: FnMut(&T) -> U>(&self, mut f: F) -> AbilityMapEntry<U> {
        match self {
            AbilityMapEntry::AbilitySet(ability_set) => {
                AbilityMapEntry::AbilitySet(ability_set.map_ref(f))
            },
            AbilityMapEntry::AbilitySetOverride {
                parent,
                guard,
                primary,
                secondary,
                added_abilities,
                removed_abilities,
            } => AbilityMapEntry::AbilitySetOverride {
                parent: parent.clone(),
                guard: guard.as_ref().map(|g| g.map_ref(&mut f)),
                primary: primary.as_ref().map(|p| p.map_ref(&mut f)),
                secondary: secondary.as_ref().map(|s| s.map_ref(&mut f)),
                added_abilities: added_abilities.iter().map(|x| x.map_ref(&mut f)).collect(),
                removed_abilities: removed_abilities
                    .iter()
                    .map(|x| x.map_ref(&mut f))
                    .collect(),
            },
        }
    }

    pub fn inherit(self, parent: &Self) -> Self {
        match self {
            AbilityMapEntry::AbilitySet(_) => self,
            AbilityMapEntry::AbilitySetOverride {
                guard,
                primary,
                secondary,
                mut added_abilities,
                mut removed_abilities,
                ..
            } => match parent {
                AbilityMapEntry::AbilitySet(parent) => {
                    added_abilities.extend(
                        parent
                            .abilities
                            .iter()
                            .filter(|x| !removed_abilities.contains(x))
                            .cloned(),
                    );

                    AbilityMapEntry::AbilitySet(AbilitySet {
                        guard: guard.or(parent.guard.clone()),
                        primary: primary.unwrap_or(parent.primary.clone()),
                        secondary: secondary.unwrap_or(parent.secondary.clone()),
                        abilities: added_abilities,
                    })
                },
                AbilityMapEntry::AbilitySetOverride {
                    parent: p_parent,
                    guard: p_guard,
                    primary: p_primary,
                    secondary: p_secondary,
                    added_abilities: p_added_abilities,
                    removed_abilities: p_removed_abilities,
                } => {
                    added_abilities.extend(
                        p_added_abilities
                            .iter()
                            .filter(|x| !removed_abilities.contains(x))
                            .cloned(),
                    );
                    removed_abilities.extend(
                        p_removed_abilities
                            .iter()
                            .filter(|x| !added_abilities.contains(x))
                            .cloned(),
                    );

                    AbilityMapEntry::AbilitySetOverride {
                        parent: p_parent.clone(),
                        guard: guard.or(p_guard.clone()),
                        primary: primary.or(p_primary.clone()),
                        secondary: secondary.or(p_secondary.clone()),
                        added_abilities,
                        removed_abilities,
                    }
                },
            },
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct AbilityMap<T = AbilityItem>(HashMap<AbilitySpec, AbilityMapEntry<T>>);

impl AbilityMap {
    pub fn load() -> AssetHandle<Self> {
        Self::load_expect("common.abilities.ability_set_manifest")
    }
}

impl<T> AbilityMap<T> {
    pub fn get_ability_set(&self, key: &AbilitySpec) -> Option<&AbilitySet<T>> {
        self.0.get(key).and_then(|entry| match entry {
            AbilityMapEntry::AbilitySet(ability_set) => Some(ability_set),
            AbilityMapEntry::AbilitySetOverride { .. } => None,
        })
    }
}

impl Asset for AbilityMap {
    fn load(cache: &AssetCache, specifier: &SharedString) -> Result<Self, BoxedError> {
        let mut ability_map = cache
            .load::<Ron<AbilityMap<String>>>(specifier)?
            .read()
            .0
            .0
            .clone();

        // Find child entries and inherit from their parent
        while let Some((spec, mut entry)) = {
            let spec = ability_map
                .iter()
                .find(|(_, entry)| matches!(entry, AbilityMapEntry::AbilitySetOverride { .. }))
                .map(|(spec, _)| spec.clone());

            spec.and_then(|spec| ability_map.remove_entry(&spec))
        } {
            let parent = if let AbilityMapEntry::AbilitySetOverride { parent, .. } = &entry {
                Some(parent)
            } else {
                None
            }
            .and_then(|parent| ability_map.get(parent));

            if let Some(parent) = parent {
                entry = entry.inherit(parent);
            }

            ability_map.insert(spec, entry);
        }

        // Xindeler: every catalogued spell becomes a `Custom(<spell id>)`
        // ability set, derived from the spell compendium rather than
        // hand-written in the manifest, so the compendium's `ability` field
        // stays the single source of truth as the catalogue grows. Injected
        // here -- after the override inheritance fixpoint, before the
        // `String -> AbilityItem` resolution below -- so the entries pick up
        // the same RON resolution and missing-file warning as every
        // hand-written one. Loaded through `cache` (not the global asset
        // cache) so hot-reload dependency tracking works.
        //
        // On a compendium load failure nothing is injected and every spell key
        // resolves to nothing at use time, which is the correct fail-closed
        // behaviour.
        if let Ok(compendium) =
            cache.load::<crate::comp::spell::SpellCompendium>("common.spells.compendium")
        {
            for spell in compendium.read().iter() {
                // `or_insert_with`: a hand-written manifest entry for the same
                // key always wins, an escape hatch for a spell that ever needs
                // a bespoke set.
                ability_map
                    .entry(AbilitySpec::Custom(spell.id.clone()))
                    .or_insert_with(|| {
                        AbilityMapEntry::AbilitySet(AbilitySet {
                            guard: None,
                            // No `Skill` gate: spell access is gated by class
                            // level in `AbilityPool::is_unlocked`, not by a
                            // spent skill point.
                            primary: AbilityKind::Simple(None, spell.ability.clone()),
                            secondary: AbilityKind::Simple(None, spell.ability.clone()),
                            abilities: Vec::new(),
                        })
                    });
            }
        }

        Ok(AbilityMap(
            ability_map
                .into_iter()
                .map(|(kind, set)| {
                    (
                        kind.clone(),
                        set.map_ref(|s| AbilityItem {
                            id: s.clone(),
                            ability: if let Ok(handle) = cache.load::<Ron<CharacterAbility>>(s) {
                                handle.cloned().into_inner()
                            } else {
                                warn!(?s, "missing specified ability file");
                                CharacterAbility::default()
                            },
                        }),
                    )
                })
                .collect::<HashMap<_, _>>(),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every `ToolKind` variant maps to the expected default role,
    /// hand-written (not derived from an iterator) so a new variant added
    /// without extending this table is caught here instead of silently
    /// falling through `default_role`'s own exhaustive match at compile
    /// time only.
    #[test]
    fn default_role_covers_every_tool_kind() {
        let expected = [
            (ToolKind::Sword, WeaponRole::Martial),
            (ToolKind::Axe, WeaponRole::Martial),
            (ToolKind::Hammer, WeaponRole::Martial),
            (ToolKind::Bow, WeaponRole::Martial),
            (ToolKind::Staff, WeaponRole::Caster),
            (ToolKind::Sceptre, WeaponRole::Caster),
            (ToolKind::Tome, WeaponRole::Caster),
            (ToolKind::HolySymbol, WeaponRole::Caster),
            (ToolKind::Focus, WeaponRole::Caster),
            (ToolKind::Dagger, WeaponRole::Martial),
            (ToolKind::Shield, WeaponRole::Martial),
            (ToolKind::Spear, WeaponRole::Martial),
            (ToolKind::Blowgun, WeaponRole::Martial),
            (ToolKind::Debug, WeaponRole::Martial),
            (ToolKind::Farming, WeaponRole::Martial),
            (ToolKind::Pick, WeaponRole::Martial),
            (ToolKind::Shovel, WeaponRole::Martial),
            (ToolKind::Instrument, WeaponRole::Martial),
            (ToolKind::Throwable, WeaponRole::Martial),
            (ToolKind::Natural, WeaponRole::Martial),
            (ToolKind::Empty, WeaponRole::Martial),
        ];
        assert_eq!(
            expected.len(),
            21,
            "a ToolKind variant is missing from this table"
        );
        for (kind, role) in expected {
            assert_eq!(
                kind.default_role(),
                role,
                "{kind:?} has an unexpected default role"
            );
        }
    }

    /// A `Tool` RON that omits `role:` resolves to its kind's default role,
    /// and an explicit `role:` overrides it -- exercised through the actual
    /// RON deserializer, not by constructing the struct directly, so a
    /// `#[serde(default)]` regression would be caught here.
    #[test]
    fn tool_role_ron_defaults_to_kind_and_can_be_overridden() {
        let no_role: Tool = ron::de::from_str(
            "(kind: Staff, hands: Two, stats: (equip_time_secs: 0.4, power: 1.0, effect_power: \
             1.0, speed: 1.0, range: 1.0, energy_efficiency: 1.0, buff_strength: 1.0))",
        )
        .expect("role: is optional");
        assert_eq!(no_role.role(), WeaponRole::Caster);

        let explicit_role: Tool = ron::de::from_str(
            "(kind: Staff, hands: Two, role: Some(Martial), stats: (equip_time_secs: 0.4, power: \
             1.0, effect_power: 1.0, speed: 1.0, range: 1.0, energy_efficiency: 1.0, \
             buff_strength: 1.0))",
        )
        .expect("explicit role: must parse");
        assert_eq!(explicit_role.role(), WeaponRole::Martial);
    }

    /// Every catalogued spell is reachable as a `Custom(<spell id>)` ability
    /// set, resolving to the `CharacterAbility` RON the compendium names.
    #[test]
    fn every_compendium_spell_resolves_through_the_ability_map() {
        let map = AbilityMap::load();
        let map = map.read();
        let book = crate::comp::spell::SpellCompendium::load_expect_cloned();
        assert!(!book.is_empty());
        for spell in book.iter() {
            let set = map
                .get_ability_set(&AbilitySpec::Custom(spell.id.clone()))
                .unwrap_or_else(|| panic!("no ability set for spell {}", spell.id));
            match &set.primary {
                AbilityKind::Simple(skill, item) => {
                    assert!(
                        skill.is_none(),
                        "spells are gated by class level, not Skill"
                    );
                    assert_eq!(item.id, spell.ability, "wrong ability RON for {}", spell.id);
                    // A `CharacterAbility::default()` here means the RON was
                    // missing and the resolution silently fell back.
                    assert_ne!(
                        item.ability,
                        CharacterAbility::default(),
                        "spell {} resolved to the default (missing RON?)",
                        spell.id
                    );
                },
                other => panic!("spell {} got a non-Simple set: {other:?}", spell.id),
            }
        }
    }

    /// The hand-written manifest entries always win over the injected ones.
    #[test]
    fn injection_never_overwrites_a_hand_written_manifest_entry() {
        let map = AbilityMap::load();
        let map = map.read();
        let set = map
            .get_ability_set(&AbilitySpec::Custom("class.mage.arcanesurge".into()))
            .expect("hand-written class key still present");
        assert!(
            matches!(&set.primary, AbilityKind::Simple(Some(_), _)),
            "keeps its Skill gate"
        );
    }
}
