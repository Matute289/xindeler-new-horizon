//! The `DerivedStats` event-driven gear/skill/body stat cache.
//!
//! Every gear-derived aggregate used to be recomputed by walking the entity's
//! loadout at each read site — four of those walks happened unconditionally
//! per entity per tick (and, on the client, per *frame*), plus six more per
//! nameplate for `combat_rating`. `DerivedStats` folds all of them into one
//! component that a dedicated system
//! (`common/systems/src/derived_stats.rs`) rebuilds **only** for entities
//! whose `Inventory` / `AttunedItems` / `SkillSet` / `Body` was actually
//! modified, using the `ComponentEvent` channels those four
//! `DerefFlaggedStorage`s already emit.
//!
//! ⛔ **Never net-synced.** Both the client and the server derive this locally
//! from components they already hold; adding it to `synced_components!` would
//! create a second source of truth for numbers both sides already derive
//! independently, for zero gain.

use specs::{
    BitSet, Component, ReadStorage, World, WorldExt, shrev::EventChannel, storage::ComponentEvent,
};

use crate::comp::{
    AttunedItems, Body, Inventory, MagicSource, SkillSet,
    attunement::item_effects_active,
    inventory::{
        item::{Item, ItemDesc, ItemKind, MaterialStatManifest, armor::Protection, tool},
        slot::EquipSlot,
    },
    skillset::SkillGroupKind,
};

/// The per-`MagicSource` and flat caster channels folded out of equipped
/// caster-role implements. Mirrors exactly what
/// `combat::apply_gear_caster_stats` writes straight onto `comp::Stats`.
#[derive(Clone, Debug, PartialEq)]
pub struct CasterGearFold {
    pub spell_power: f32,
    pub spell_power_by_source: [f32; MagicSource::NUM_SOURCES],
    pub heal_power: f32,
    pub energy_regen_modifier: f32,
    pub cooldown_reduction_modifier: f32,
}

impl Default for CasterGearFold {
    /// All channels are MULTIPLICATIVE — identity is `1.0`, not `0.0`. These
    /// are the same identities `comp::Stats::empty` starts every one of these
    /// fields at, which is what the fold multiplies onto.
    fn default() -> Self {
        Self {
            spell_power: 1.0,
            spell_power_by_source: [1.0; MagicSource::NUM_SOURCES],
            heal_power: 1.0,
            energy_regen_modifier: 1.0,
            cooldown_reduction_modifier: 1.0,
        }
    }
}

/// Everything derivable from an entity's loadout + skillset + body.
///
/// Rebuilt ONLY when `Inventory`, `AttunedItems`, `SkillSet` or `Body` is
/// modified (see `common/systems/src/derived_stats.rs`). Never net-synced:
/// both the client and the server derive it locally from components they
/// already hold.
#[derive(Clone, Debug, PartialEq)]
pub struct DerivedStats {
    // --- attunement-aware: what combat resolution uses ---
    /// `None` == the entity's armour makes it invulnerable.
    pub protection: Option<f32>,
    pub max_energy_mod: f32,

    // --- attunement-blind: what `combat_rating` uses ---
    pub protection_unattuned: Option<f32>,
    pub max_energy_mod_unattuned: f32,

    // --- attunement-independent ---
    pub poise_resilience: Option<f32>,
    pub precision_mult: f32,
    pub energy_reward_mod: f32,
    pub stealth: f32,
    pub weapon_rating: f32,
    pub weapon_kinds: (Option<tool::ToolKind>, Option<tool::ToolKind>),
    pub weapon_skill_groups: (Option<SkillGroupKind>, Option<SkillGroupKind>),
    pub caster: CasterGearFold,

    /// The finished combat rating, cached whole rather than as a gear half
    /// plus a live tail: every one of its remaining inputs (`SkillSet`,
    /// `Body`) is in this component's invalidation set already, and its
    /// `Health`/`Energy`/`Poise` inputs are `base_max()` reads, which are
    /// fixed for an entity's lifetime.
    pub combat_rating: f32,
}

impl Default for DerivedStats {
    /// ⚠️ **Load-bearing.** Every field here MUST equal what the corresponding
    /// `combat::compute_*` free function returns for `inventory: None`,
    /// because an entity with no `DerivedStats` falls back to exactly these
    /// values at every read site. In particular:
    ///
    /// - `precision_mult` is **1.1**, not 1.0 (`compute_precision_mult` is `1.0
    ///   + inventory.map_or(0.1, …)`).
    /// - `protection` / `poise_resilience` are **`Some(0.0)`**, not `None` —
    ///   `None` specifically means *invincible*.
    ///
    /// `default_matches_the_no_inventory_results` pins all of them against the
    /// free functions.
    fn default() -> Self {
        Self {
            protection: Some(0.0),
            max_energy_mod: 0.0,
            protection_unattuned: Some(0.0),
            max_energy_mod_unattuned: 0.0,
            poise_resilience: Some(0.0),
            // `1.0 + 0.1_f32.max(0.0)`, constant-folded. Written as the sum
            // rather than as the literal `1.1` so the provenance of the value
            // stays visible.
            precision_mult: 1.0 + 0.1,
            energy_reward_mod: 1.0,
            stealth: 0.0,
            weapon_rating: 0.0,
            weapon_kinds: (None, None),
            weapon_skill_groups: (None, None),
            caster: CasterGearFold::default(),
            combat_rating: 0.0,
        }
    }
}

impl Component for DerivedStats {
    /// Plain `VecStorage` on purpose: nothing tracks changes *to* this
    /// component, so a `DerefFlaggedStorage` would only cost event traffic for
    /// no consumer.
    type Storage = specs::VecStorage<Self>;
}

impl DerivedStats {
    /// Rebuild the whole cache for one entity.
    ///
    /// The arithmetic in here is a **verbatim** port of the `combat::compute_*`
    /// family — same operations in the same order, so the results are
    /// bit-for-bit identical to what those free functions produce. The
    /// equivalence tests at the bottom of this file assert that with exact
    /// `f32` equality, not an epsilon. Do not "clean up" or reorder any of it.
    ///
    /// `combat_rating` is only computed when *all* of its inputs are present
    /// (an inventory, a skillset, a body and the three base maxima); otherwise
    /// it stays at the `Default` value of `0.0`, matching the fact that
    /// `combat::combat_rating` requires all of them by reference.
    pub fn compute(
        inventory: Option<&Inventory>,
        attuned: Option<&AttunedItems>,
        skill_set: Option<&SkillSet>,
        body: Option<Body>,
        health_base_max: Option<f32>,
        energy_base_max: Option<f32>,
        poise_base_max: Option<f32>,
        msm: &MaterialStatManifest,
    ) -> Self {
        let protection = protection_from(inventory, attuned, msm);
        let protection_unattuned = protection_from(inventory, None, msm);
        let max_energy_mod = max_energy_mod_from(inventory, attuned, msm);
        let max_energy_mod_unattuned = max_energy_mod_from(inventory, None, msm);
        let poise_resilience = poise_resilience_from(inventory, msm);
        let precision_mult = precision_mult_from(inventory, msm);
        let energy_reward_mod = energy_reward_mod_from(inventory, msm);
        let stealth = stealth_from(inventory, msm);
        let weapon_rating = inventory.map_or(0.0, |inv| weapon_rating_from(inv, msm));
        let weapon_kinds = inventory.map_or((None, None), weapon_kinds_from);
        let weapon_skill_groups = inventory.map_or((None, None), weapon_skill_groups_from);
        let caster = caster_fold_from(inventory, attuned);

        // `combat_rating` is deliberately attunement-BLIND: `combat_rating`
        // passes `None` for `attuned` at every internal call site, so it reads
        // the `_unattuned` variants here.
        let combat_rating = match (
            inventory,
            skill_set,
            body,
            health_base_max,
            energy_base_max,
            poise_base_max,
        ) {
            (
                Some(_inventory),
                Some(skill_set),
                Some(body),
                Some(health_base_max),
                Some(energy_base_max),
                Some(poise_base_max),
            ) => combat_rating_from(CombatRatingTerms {
                skill_set,
                body,
                health_base_max,
                energy_base_max,
                poise_base_max,
                protection_unattuned,
                max_energy_mod_unattuned,
                energy_reward_mod,
                poise_resilience,
                precision_mult,
                weapon_skill_groups,
                weapon_rating,
            }),
            _ => 0.0,
        };

        Self {
            protection,
            max_energy_mod,
            protection_unattuned,
            max_energy_mod_unattuned,
            poise_resilience,
            precision_mult,
            energy_reward_mod,
            stealth,
            weapon_rating,
            weapon_kinds,
            weapon_skill_groups,
            caster,
            combat_rating,
        }
    }
}

// ---------------------------------------------------------------------------
// The `combat::compute_*` bodies, ported here verbatim.
//
// These are private because `DerivedStats::compute` is the only supported way
// to obtain any of them from now on -- the whole point of the cache is that
// the loadout is walked once, at rebuild time, instead of once per read site.
// ---------------------------------------------------------------------------

/// Verbatim `combat::compute_protection`.
fn protection_from(
    inventory: Option<&Inventory>,
    attuned: Option<&AttunedItems>,
    msm: &MaterialStatManifest,
) -> Option<f32> {
    inventory.map_or(Some(0.0), |inv| {
        inv.equipped_items_with_slot()
            .filter(|(slot, item)| item_effects_active(*slot, item.requires_attunement(), attuned))
            .filter_map(|(_, item)| {
                if let ItemKind::Armor(armor) = &*item.kind() {
                    armor
                        .stats(msm, item.stats_durability_multiplier())
                        .protection
                } else {
                    None
                }
            })
            .map(|protection| match protection {
                Protection::Normal(protection) => Some(protection),
                Protection::Invincible => None,
            })
            .sum::<Option<f32>>()
    })
}

/// Verbatim `combat::compute_max_energy_mod`.
fn max_energy_mod_from(
    inventory: Option<&Inventory>,
    attuned: Option<&AttunedItems>,
    msm: &MaterialStatManifest,
) -> f32 {
    inventory.map_or(0.0, |inv| {
        inv.equipped_items_with_slot()
            .filter(|(slot, item)| item_effects_active(*slot, item.requires_attunement(), attuned))
            .filter_map(|(_, item)| {
                if let ItemKind::Armor(armor) = &*item.kind() {
                    armor
                        .stats(msm, item.stats_durability_multiplier())
                        .energy_max
                } else {
                    None
                }
            })
            .sum()
    })
}

/// Verbatim `combat::compute_poise_resilience`.
fn poise_resilience_from(inventory: Option<&Inventory>, msm: &MaterialStatManifest) -> Option<f32> {
    inventory.map_or(Some(0.0), |inv| {
        inv.equipped_items()
            .filter_map(|item| {
                if let ItemKind::Armor(armor) = &*item.kind() {
                    armor
                        .stats(msm, item.stats_durability_multiplier())
                        .poise_resilience
                } else {
                    None
                }
            })
            .map(|protection| match protection {
                Protection::Normal(protection) => Some(protection),
                Protection::Invincible => None,
            })
            .sum::<Option<f32>>()
    })
}

/// Verbatim `combat::compute_precision_mult`.
fn precision_mult_from(inventory: Option<&Inventory>, msm: &MaterialStatManifest) -> f32 {
    // Starts with a value of 0.1 when summing the stats from each armor piece, and
    // defaults to a value of 0.1 if no inventory is equipped. Precision multiplier
    // cannot go below 1
    1.0 + inventory
        .map_or(0.1, |inv| {
            inv.equipped_items()
                .filter_map(|item| {
                    if let ItemKind::Armor(armor) = &*item.kind() {
                        armor
                            .stats(msm, item.stats_durability_multiplier())
                            .precision_power
                    } else {
                        None
                    }
                })
                .fold(0.1, |a, b| a + b)
        })
        .max(0.0)
}

/// Verbatim `combat::compute_energy_reward_mod`.
fn energy_reward_mod_from(inventory: Option<&Inventory>, msm: &MaterialStatManifest) -> f32 {
    // Starts with a value of 1.0 when summing the stats from each armor piece, and
    // defaults to a value of 1.0 if no inventory is present
    inventory.map_or(1.0, |inv| {
        inv.equipped_items()
            .filter_map(|item| {
                if let ItemKind::Armor(armor) = &*item.kind() {
                    armor
                        .stats(msm, item.stats_durability_multiplier())
                        .energy_reward
                } else {
                    None
                }
            })
            .fold(1.0, |a, b| a + b)
    })
}

/// Verbatim `combat::compute_stealth`.
fn stealth_from(inventory: Option<&Inventory>, msm: &MaterialStatManifest) -> f32 {
    inventory.map_or(0.0, |inv| {
        inv.equipped_items()
            .filter_map(|item| {
                if let ItemKind::Armor(armor) = &*item.kind() {
                    armor.stats(msm, item.stats_durability_multiplier()).stealth
                } else {
                    None
                }
            })
            .sum()
    })
}

/// Verbatim `combat::get_weapon_kinds`.
fn weapon_kinds_from(inv: &Inventory) -> (Option<tool::ToolKind>, Option<tool::ToolKind>) {
    (
        inv.equipped(EquipSlot::ActiveMainhand).and_then(|i| {
            if let ItemKind::Tool(tool) = &*i.kind() {
                Some(tool.kind)
            } else {
                None
            }
        }),
        inv.equipped(EquipSlot::ActiveOffhand).and_then(|i| {
            if let ItemKind::Tool(tool) = &*i.kind() {
                Some(tool.kind)
            } else {
                None
            }
        }),
    )
}

/// The gear half of `combat::weapon_skills` — which `SkillGroupKind` each
/// active hand's equipped tool feeds. The `SkillSet` lookup itself stays live
/// (it is folded into `combat_rating` below), because it is a cheap map read
/// and not a loadout walk.
fn weapon_skill_groups_from(inv: &Inventory) -> (Option<SkillGroupKind>, Option<SkillGroupKind>) {
    let equipped_tool_group = |slot| {
        inv.equipped(slot).and_then(|item| {
            if let ItemKind::Tool(tool) = &*item.kind() {
                Some(crate::combat::skill_group_for_weapon(tool))
            } else {
                None
            }
        })
    };
    (
        equipped_tool_group(EquipSlot::ActiveMainhand),
        equipped_tool_group(EquipSlot::ActiveOffhand),
    )
}

/// Verbatim `combat::weapon_rating`, specialised to `&Item` (its generic
/// `ItemDesc` parameter only ever had one caller shape).
fn item_weapon_rating(item: &Item) -> f32 {
    const POWER_WEIGHT: f32 = 2.0;
    const SPEED_WEIGHT: f32 = 3.0;
    const RANGE_WEIGHT: f32 = 0.8;
    const EFFECT_WEIGHT: f32 = 1.5;
    const EQUIP_TIME_WEIGHT: f32 = 0.0;
    const ENERGY_EFFICIENCY_WEIGHT: f32 = 1.5;
    const BUFF_STRENGTH_WEIGHT: f32 = 1.5;

    let rating = if let ItemKind::Tool(tool) = &*item.kind() {
        let stats = tool.stats(item.stats_durability_multiplier());

        let power_rating = stats.power;
        let speed_rating = stats.speed - 1.0;
        let range_rating = stats.range - 1.0;
        let effect_rating = stats.effect_power - 1.0;
        let equip_time_rating = 0.5 - stats.equip_time_secs;
        let energy_efficiency_rating = stats.energy_efficiency - 1.0;
        let buff_strength_rating = stats.buff_strength - 1.0;

        power_rating * POWER_WEIGHT
            + speed_rating * SPEED_WEIGHT
            + range_rating * RANGE_WEIGHT
            + effect_rating * EFFECT_WEIGHT
            + equip_time_rating * EQUIP_TIME_WEIGHT
            + energy_efficiency_rating * ENERGY_EFFICIENCY_WEIGHT
            + buff_strength_rating * BUFF_STRENGTH_WEIGHT
    } else {
        0.0
    };
    rating.max(0.0)
}

/// Verbatim `combat::get_weapon_rating`.
fn weapon_rating_from(inventory: &Inventory, _msm: &MaterialStatManifest) -> f32 {
    let mainhand_rating = if let Some(item) = inventory.equipped(EquipSlot::ActiveMainhand) {
        item_weapon_rating(item)
    } else {
        0.0
    };

    let offhand_rating = if let Some(item) = inventory.equipped(EquipSlot::ActiveOffhand) {
        item_weapon_rating(item)
    } else {
        0.0
    };

    mainhand_rating.max(offhand_rating)
}

/// Verbatim `combat::apply_gear_caster_stats`, folding onto a fresh
/// `CasterGearFold` (whose identities are the same `1.0`s `comp::Stats` starts
/// those channels at) instead of mutating a `Stats` in place.
fn caster_fold_from(
    inventory: Option<&Inventory>,
    attuned: Option<&AttunedItems>,
) -> CasterGearFold {
    let mut fold = CasterGearFold::default();
    let Some(inventory) = inventory else {
        return fold;
    };
    for (slot, item) in inventory.equipped_items_with_slot() {
        if !item_effects_active(slot, item.requires_attunement(), attuned) {
            continue;
        }
        let ItemKind::Tool(tool) = &*item.kind() else {
            continue;
        };
        if tool.role() != tool::WeaponRole::Caster {
            continue;
        }
        let tool_stats = tool.stats(item.stats_durability_multiplier());
        match tool.spell_power_source() {
            Some(source) => fold.spell_power_by_source[source.index()] *= tool_stats.effect_power,
            None => fold.spell_power *= tool_stats.effect_power,
        }
        fold.heal_power *= tool_stats.buff_strength;
        fold.energy_regen_modifier *= tool_stats.energy_efficiency;
        fold.cooldown_reduction_modifier *= tool_stats.cooldown_reduction;
    }
    fold
}

/// The `(damage: None, attuned: None, stats: None)` shape of
/// `Damage::compute_damage_reduction` that `combat::combat_rating` calls.
///
/// Three branches of the original are constant-folded here, each bit-exactly:
/// `damage: None` ⇒ `penetration = 0.0` and `protection.map(|p| p - 0.0)` is
/// the identity for every `f32` (including `-0.0`); `stats: None` ⇒
/// `stats_dr = 0.0`, so `stats_dr >= 1.0` is always false and `* (1.0 - 0.0)`
/// is a multiplication by exactly `1.0`.
///
/// ⚠️ The surviving `1.0 - (1.0 - inventory_dr)` is deliberately **not**
/// simplified to `inventory_dr`: for a small `inventory_dr` the round trip
/// through `1.0 -` is lossy, and this cache must stay bit-for-bit equivalent
/// to the arithmetic it replaces, quirks included.
fn combat_rating_damage_reduction(protection: Option<f32>) -> f32 {
    const FIFTY_PERCENT_DR_THRESHOLD: f32 = 60.0;

    let inventory_dr = match protection {
        Some(dr) => dr / (FIFTY_PERCENT_DR_THRESHOLD + dr.abs()),
        None => 1.0,
    };

    if protection.is_none() {
        1.0
    } else {
        1.0 - (1.0 - inventory_dr)
    }
}

/// The `(char_state: None, stats: None)` shape of
/// `Poise::compute_poise_damage_reduction` that `combat::combat_rating` calls.
/// `from_char` and `from_stats` are both `0.0` under those arguments, so their
/// `(1.0 - x)` factors are multiplications by exactly `1.0`. The
/// `1.0 - (1.0 - from_inventory)` round trip is preserved for the same reason
/// as in [`combat_rating_damage_reduction`].
fn combat_rating_poise_damage_reduction(poise_resilience: Option<f32>) -> f32 {
    let from_inventory = match poise_resilience {
        Some(dr) => dr / (60.0 + dr.abs()),
        None => 1.0,
    };
    1.0 - (1.0 - from_inventory)
}

/// Everything `combat_rating_from` needs, grouped so the (long) argument list
/// stays readable and impossible to permute by accident.
struct CombatRatingTerms<'a> {
    skill_set: &'a SkillSet,
    body: Body,
    health_base_max: f32,
    energy_base_max: f32,
    poise_base_max: f32,
    protection_unattuned: Option<f32>,
    max_energy_mod_unattuned: f32,
    energy_reward_mod: f32,
    poise_resilience: Option<f32>,
    precision_mult: f32,
    weapon_skill_groups: (Option<SkillGroupKind>, Option<SkillGroupKind>),
    weapon_rating: f32,
}

/// Verbatim `combat::combat_rating`, with each of its six loadout walks
/// replaced by the already-computed value it used to recompute.
fn combat_rating_from(terms: CombatRatingTerms<'_>) -> f32 {
    const WEAPON_WEIGHT: f32 = 1.0;
    const HEALTH_WEIGHT: f32 = 1.5;
    const ENERGY_WEIGHT: f32 = 0.5;
    const SKILLS_WEIGHT: f32 = 1.0;
    const POISE_WEIGHT: f32 = 0.5;
    const PRECISION_WEIGHT: f32 = 0.5;

    let CombatRatingTerms {
        skill_set,
        body,
        health_base_max,
        energy_base_max,
        poise_base_max,
        protection_unattuned,
        max_energy_mod_unattuned,
        energy_reward_mod,
        poise_resilience,
        precision_mult,
        weapon_skill_groups,
        weapon_rating,
    } = terms;

    // Normalized with a standard max health of 100
    let health_rating = health_base_max
        / 100.0
        / (1.0 - combat_rating_damage_reduction(protection_unattuned)).max(0.00001);

    // Normalized with a standard max energy of 100 and energy reward multiplier of
    // x1
    let energy_rating = (energy_base_max + max_energy_mod_unattuned) / 100.0 * energy_reward_mod;

    // Normalized with a standard max poise of 100
    let poise_rating = poise_base_max
        / 100.0
        / (1.0 - combat_rating_poise_damage_reduction(poise_resilience)).max(0.00001);

    // Normalized with a standard precision multiplier of 1.2
    let precision_rating = precision_mult / 1.2;

    // The live half of `combat::weapon_skills`: which group each hand feeds is
    // gear-derived (and cached in `weapon_skill_groups`); how many points have
    // been earned in it is a cheap `SkillSet` map read.
    let mainhand_skills = weapon_skill_groups
        .0
        .map_or(0.0, |group| skill_set.earned_sp(group) as f32);
    let offhand_skills = weapon_skill_groups
        .1
        .map_or(0.0, |group| skill_set.earned_sp(group) as f32);
    let weapon_skills = mainhand_skills.max(offhand_skills);

    // Assumes a standard person has earned 20 skill points in the general skill
    // tree and 10 skill points for the weapon skill tree
    let skills_rating =
        (skill_set.earned_sp(SkillGroupKind::General) as f32 / 20.0 + weapon_skills / 10.0) / 2.0;

    let combined_rating = (health_rating * HEALTH_WEIGHT
        + energy_rating * ENERGY_WEIGHT
        + poise_rating * POISE_WEIGHT
        + precision_rating * PRECISION_WEIGHT
        + skills_rating * SKILLS_WEIGHT
        + weapon_rating * WEAPON_WEIGHT)
        / (HEALTH_WEIGHT
            + ENERGY_WEIGHT
            + POISE_WEIGHT
            + PRECISION_WEIGHT
            + SKILLS_WEIGHT
            + WEAPON_WEIGHT);

    // Body multiplier meant to account for an enemy being harder than equipment and
    // skills would account for. It should only not be 1.0 for non-humanoids
    combined_rating * body.combat_multiplier()
}

// ---------------------------------------------------------------------------
// The tracker resource
// ---------------------------------------------------------------------------

/// Holds the four `ReaderId`s whose `ComponentEvent` channels invalidate
/// [`DerivedStats`], plus the scratch `BitSet` the rebuild system drains them
/// into.
///
/// ⚠️ **Why this is a resource and not system state.** `common_ecs::System`
/// (`common/ecs/src/system.rs`) has no `setup()` hook and `Job<T>` does not
/// forward `specs::System::setup`, so a `ReaderId` cannot be lazily registered
/// from inside the system the way stock specs code usually does it. It is
/// registered once in `common_state::State::setup_ecs_world`, *after* the four
/// source components are registered — exactly the pattern
/// `common/net/src/sync/track.rs::UpdateTracker` already uses.
///
/// ⚠️ **`Default` is deliberately NOT implemented.** There is no valid default
/// `ReaderId`, and a `Write<'a, T>` that auto-defaulted would silently register
/// nothing and leave the cache permanently stale. The system fetches this with
/// `WriteExpect`, so a missing resource panics loudly at startup instead.
pub struct DerivedStatsTrackers {
    inventory: specs::ReaderId<ComponentEvent>,
    attuned: specs::ReaderId<ComponentEvent>,
    skill_set: specs::ReaderId<ComponentEvent>,
    body: specs::ReaderId<ComponentEvent>,
    /// Entities whose cache must be rebuilt (or dropped) this tick.
    dirty: BitSet,
    /// Diagnostic counter: how many entity caches have been rebuilt since
    /// startup. Deliberately not `#[cfg(test)]`-gated, because
    /// `common/systems/tests/derived_stats.rs` lives in a different crate and
    /// this is the only way it can assert the central claim of the cache —
    /// that a steady-state tick rebuilds *nothing*. A `u64` increment on an
    /// already-rare path costs nothing.
    rebuilds: u64,
}

impl DerivedStatsTrackers {
    /// Registers one reader per invalidating storage. Must be called *after*
    /// `Inventory`, `AttunedItems`, `SkillSet` and `Body` are registered —
    /// `register_reader` needs the storages to exist.
    pub fn new(world: &World) -> Self {
        Self {
            inventory: world.write_storage::<Inventory>().register_reader(),
            attuned: world.write_storage::<AttunedItems>().register_reader(),
            skill_set: world.write_storage::<SkillSet>().register_reader(),
            body: world.write_storage::<Body>().register_reader(),
            dirty: BitSet::new(),
            rebuilds: 0,
        }
    }

    /// Drains all four `ComponentEvent` channels into a single dirty `BitSet`.
    ///
    /// `Inserted`, `Modified` and `Removed` all land in the same set: a removal
    /// changes the derived result just as much as a modification does, and the
    /// system decides what to do with each id by looking at whether the entity
    /// still has an `Inventory`. That is strictly more robust than a
    /// per-channel removal set would be — a removed `SkillSet` must *rebuild*,
    /// not drop, the cache of an entity that still has gear.
    ///
    /// Draining once per tick also batches by construction: however many
    /// mutations hit one entity during a tick, it is rebuilt exactly once.
    pub fn record_changes(
        &mut self,
        inventories: &ReadStorage<'_, Inventory>,
        attuned_items: &ReadStorage<'_, AttunedItems>,
        skill_sets: &ReadStorage<'_, SkillSet>,
        bodies: &ReadStorage<'_, Body>,
    ) {
        self.dirty.clear();
        drain(&mut self.dirty, inventories.channel(), &mut self.inventory);
        drain(&mut self.dirty, attuned_items.channel(), &mut self.attuned);
        drain(&mut self.dirty, skill_sets.channel(), &mut self.skill_set);
        drain(&mut self.dirty, bodies.channel(), &mut self.body);
    }

    pub fn dirty(&self) -> &BitSet { &self.dirty }

    /// How many entity caches have been rebuilt since startup.
    pub fn rebuilds(&self) -> u64 { self.rebuilds }

    /// Called by the rebuild system once per entity it actually recomputes.
    pub fn note_rebuild(&mut self) { self.rebuilds += 1; }
}

fn drain(
    dirty: &mut BitSet,
    channel: &EventChannel<ComponentEvent>,
    reader: &mut specs::ReaderId<ComponentEvent>,
) {
    for event in channel.read(reader) {
        match event {
            ComponentEvent::Inserted(id)
            | ComponentEvent::Modified(id)
            | ComponentEvent::Removed(id) => {
                dirty.add(*id);
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        combat,
        comp::{
            Energy, Health, Poise, humanoid,
            inventory::{
                item::{
                    AbilityMap, Item, ItemBase, ItemDef, ItemTag,
                    armor::{self, Armor, ArmorKind},
                },
                loadout_builder::LoadoutBuilder,
                slot::ArmorSlot,
            },
        },
        skillset_builder::SkillSetBuilder,
    };
    use std::sync::Arc;

    fn msm() -> MaterialStatManifest { MaterialStatManifest::load().cloned() }

    fn test_body() -> Body { Body::Humanoid(humanoid::Body::random()) }

    fn item_from_kind(kind: ItemKind) -> Item {
        Item::new_from_item_base(
            ItemBase::Simple(Arc::new(ItemDef::create_test_itemdef_from_kind(kind))),
            Vec::new(),
            &AbilityMap::load().read(),
            &MaterialStatManifest::load().read(),
        )
    }

    fn attunement_item_from_kind(kind: ItemKind) -> Item {
        let mut item_def = ItemDef::create_test_itemdef_from_kind(kind);
        item_def.tags = vec![ItemTag::RequiresAttunement];
        Item::new_from_item_base(
            ItemBase::Simple(Arc::new(item_def)),
            Vec::new(),
            &AbilityMap::load().read(),
            &MaterialStatManifest::load().read(),
        )
    }

    fn armor_stats() -> armor::Stats {
        armor::Stats {
            protection: Some(Protection::Normal(12.5)),
            poise_resilience: Some(Protection::Normal(7.25)),
            energy_max: Some(13.0),
            energy_reward: Some(0.35),
            precision_power: Some(0.17),
            stealth: Some(0.9),
            ground_contact: Default::default(),
        }
    }

    fn chest_armor(stats: armor::Stats) -> ItemKind {
        ItemKind::Armor(Armor::new(
            ArmorKind::Chest,
            armor::StatsSource::Direct(stats),
        ))
    }

    fn caster_staff_stats() -> tool::Stats {
        tool::Stats {
            equip_time_secs: 0.4,
            power: 1.0,
            effect_power: 1.5,
            speed: 1.0,
            range: 1.0,
            energy_efficiency: 1.25,
            buff_strength: 1.75,
            cooldown_reduction: 1.0,
        }
    }

    fn tool_item(kind: tool::ToolKind, role: tool::WeaponRole, stats: tool::Stats) -> Item {
        item_from_kind(ItemKind::Tool(tool::Tool::new(
            kind,
            tool::Hands::Two,
            Some(role),
            stats,
        )))
    }

    fn attunement_required_tool_item(
        kind: tool::ToolKind,
        role: tool::WeaponRole,
        stats: tool::Stats,
    ) -> Item {
        attunement_item_from_kind(ItemKind::Tool(tool::Tool::new(
            kind,
            tool::Hands::Two,
            Some(role),
            stats,
        )))
    }

    fn empty_inventory() -> Inventory {
        Inventory::with_loadout_humanoid(LoadoutBuilder::empty().build())
    }

    fn inventory_with_mainhand(item: Item) -> Inventory {
        Inventory::with_loadout_humanoid(
            LoadoutBuilder::empty().active_mainhand(Some(item)).build(),
        )
    }

    /// A fully-kitted fixture: armour that moves every armour-derived channel
    /// plus a weapon that moves `weapon_rating` / `weapon_kinds`.
    fn geared_inventory() -> Inventory {
        Inventory::with_loadout_humanoid(
            LoadoutBuilder::empty()
                .chest(Some(item_from_kind(chest_armor(armor_stats()))))
                .active_mainhand(Some(tool_item(
                    tool::ToolKind::Sword,
                    tool::WeaponRole::Martial,
                    caster_staff_stats(),
                )))
                .build(),
        )
    }

    /// The same armour, but gated behind attunement, so the attuned and
    /// unattuned variants of every attunement-aware field actually differ.
    fn attunement_gated_inventory() -> Inventory {
        Inventory::with_loadout_humanoid(
            LoadoutBuilder::empty()
                .chest(Some(attunement_item_from_kind(chest_armor(armor_stats()))))
                .build(),
        )
    }

    fn attuned_chest() -> AttunedItems { AttunedItems(vec![EquipSlot::Armor(ArmorSlot::Chest)]) }

    fn compute_for(inventory: Option<&Inventory>, attuned: Option<&AttunedItems>) -> DerivedStats {
        let msm = msm();
        let skill_set = SkillSetBuilder::default().build();
        let body = test_body();
        DerivedStats::compute(
            inventory,
            attuned,
            Some(&skill_set),
            Some(body),
            Some(Health::new(body).base_max()),
            Some(Energy::new(body).base_max()),
            Some(Poise::new(body).base_max()),
            &msm,
        )
    }

    // -----------------------------------------------------------------
    // `Default` — every field must equal the no-inventory result of the
    // corresponding free function.
    // -----------------------------------------------------------------

    #[test]
    fn default_matches_the_no_inventory_results() {
        let msm = msm();
        let d = DerivedStats::default();

        assert_eq!(d.protection, combat::compute_protection(None, None, &msm));
        assert_eq!(
            d.protection_unattuned,
            combat::compute_protection(None, None, &msm)
        );
        assert_eq!(
            d.max_energy_mod,
            combat::compute_max_energy_mod(None, None, &msm)
        );
        assert_eq!(
            d.max_energy_mod_unattuned,
            combat::compute_max_energy_mod(None, None, &msm)
        );
        assert_eq!(
            d.poise_resilience,
            combat::compute_poise_resilience(None, &msm)
        );
        assert_eq!(d.precision_mult, combat::compute_precision_mult(None, &msm));
        assert_eq!(
            d.energy_reward_mod,
            combat::compute_energy_reward_mod(None, &msm)
        );
        assert_eq!(d.stealth, combat::compute_stealth(None, &msm));
        assert_eq!(d.weapon_rating, 0.0);
        assert_eq!(d.weapon_kinds, (None, None));
        assert_eq!(d.weapon_skill_groups, (None, None));
        assert_eq!(d.combat_rating, 0.0);
        assert_eq!(d.caster, CasterGearFold::default());

        // The two easy-to-get-wrong ones, pinned explicitly as well as by
        // equivalence: `None` would mean *invincible*, and `1.0` would be a
        // silent nerf to every precision roll.
        assert_eq!(d.protection, Some(0.0));
        assert_eq!(d.poise_resilience, Some(0.0));
        assert_eq!(d.precision_mult, 1.1);
    }

    /// `compute` with no inputs at all must land on exactly `Default` — that
    /// is what makes "no `DerivedStats` component" and "an empty one"
    /// interchangeable at every read site.
    #[test]
    fn compute_with_no_inputs_equals_default() {
        let msm = msm();
        assert_eq!(
            DerivedStats::compute(None, None, None, None, None, None, None, &msm),
            DerivedStats::default()
        );
    }

    // -----------------------------------------------------------------
    // Equivalence with the free functions — EXACT `f32` equality.
    // -----------------------------------------------------------------

    #[test]
    fn compute_equals_legacy_protection() {
        let msm = msm();
        let inv = geared_inventory();
        assert_eq!(
            compute_for(Some(&inv), None).protection,
            combat::compute_protection(Some(&inv), None, &msm)
        );

        // ...and with an attunement-gated piece, in both directions.
        let gated = attunement_gated_inventory();
        let attuned = attuned_chest();
        assert_eq!(
            compute_for(Some(&gated), None).protection,
            combat::compute_protection(Some(&gated), None, &msm)
        );
        let attuned_derived = compute_for(Some(&gated), Some(&attuned));
        assert_eq!(
            attuned_derived.protection,
            combat::compute_protection(Some(&gated), Some(&attuned), &msm)
        );
        assert_eq!(
            attuned_derived.protection_unattuned,
            combat::compute_protection(Some(&gated), None, &msm)
        );
        assert_ne!(
            attuned_derived.protection, attuned_derived.protection_unattuned,
            "the fixture must actually exercise the attunement gate"
        );
    }

    #[test]
    fn compute_equals_legacy_precision_mult() {
        let msm = msm();
        let inv = geared_inventory();
        assert_eq!(
            compute_for(Some(&inv), None).precision_mult,
            combat::compute_precision_mult(Some(&inv), &msm)
        );
        let empty = empty_inventory();
        assert_eq!(
            compute_for(Some(&empty), None).precision_mult,
            combat::compute_precision_mult(Some(&empty), &msm)
        );
    }

    #[test]
    fn compute_equals_legacy_energy_reward() {
        let msm = msm();
        let inv = geared_inventory();
        assert_eq!(
            compute_for(Some(&inv), None).energy_reward_mod,
            combat::compute_energy_reward_mod(Some(&inv), &msm)
        );
    }

    #[test]
    fn compute_equals_legacy_max_energy() {
        let msm = msm();
        let inv = geared_inventory();
        assert_eq!(
            compute_for(Some(&inv), None).max_energy_mod,
            combat::compute_max_energy_mod(Some(&inv), None, &msm)
        );

        let gated = attunement_gated_inventory();
        let attuned = attuned_chest();
        let derived = compute_for(Some(&gated), Some(&attuned));
        assert_eq!(
            derived.max_energy_mod,
            combat::compute_max_energy_mod(Some(&gated), Some(&attuned), &msm)
        );
        assert_eq!(
            derived.max_energy_mod_unattuned,
            combat::compute_max_energy_mod(Some(&gated), None, &msm)
        );
        assert_ne!(
            derived.max_energy_mod, derived.max_energy_mod_unattuned,
            "the fixture must actually exercise the attunement gate"
        );
    }

    #[test]
    fn compute_equals_legacy_stealth() {
        let msm = msm();
        let inv = geared_inventory();
        assert_eq!(
            compute_for(Some(&inv), None).stealth,
            combat::compute_stealth(Some(&inv), &msm)
        );
    }

    #[test]
    fn compute_equals_legacy_poise_resilience() {
        let msm = msm();
        let inv = geared_inventory();
        assert_eq!(
            compute_for(Some(&inv), None).poise_resilience,
            combat::compute_poise_resilience(Some(&inv), &msm)
        );
    }

    #[test]
    fn compute_equals_legacy_combat_rating() {
        let msm = msm();
        let inv = geared_inventory();
        let body = test_body();
        let health = Health::new(body);
        let energy = Energy::new(body);
        let poise = Poise::new(body);
        let mut skill_set = SkillSetBuilder::default().build();
        skill_set.grant_skill_point(SkillGroupKind::General);
        skill_set.grant_skill_point(SkillGroupKind::General);

        let derived = DerivedStats::compute(
            Some(&inv),
            None,
            Some(&skill_set),
            Some(body),
            Some(health.base_max()),
            Some(energy.base_max()),
            Some(poise.base_max()),
            &msm,
        );

        assert_eq!(
            derived.combat_rating,
            combat::combat_rating(&inv, &health, &energy, &poise, &skill_set, body, &msm)
        );
        assert!(
            derived.combat_rating > 0.0,
            "fixture must be non-degenerate"
        );
    }

    /// The gear half of `combat_rating`'s weapon terms, pinned against the
    /// helper it was split out of.
    #[test]
    fn compute_equals_legacy_weapon_kinds() {
        let inv = geared_inventory();
        assert_eq!(
            compute_for(Some(&inv), None).weapon_kinds,
            combat::get_weapon_kinds(&inv)
        );
    }

    /// An invincible armour piece must still read as `None` (== invulnerable)
    /// rather than collapsing to a number.
    #[test]
    fn invincible_armour_still_reads_as_none() {
        let msm = msm();
        let inv = Inventory::with_loadout_humanoid(
            LoadoutBuilder::empty()
                .chest(Some(item_from_kind(chest_armor(armor::Stats {
                    protection: Some(Protection::Invincible),
                    poise_resilience: Some(Protection::Invincible),
                    ..armor_stats()
                }))))
                .build(),
        );
        let derived = compute_for(Some(&inv), None);
        assert_eq!(derived.protection, None);
        assert_eq!(derived.poise_resilience, None);
        assert_eq!(
            derived.protection,
            combat::compute_protection(Some(&inv), None, &msm)
        );
        assert_eq!(
            derived.poise_resilience,
            combat::compute_poise_resilience(Some(&inv), &msm)
        );
    }

    // -----------------------------------------------------------------
    // The caster fold — the `apply_gear_caster_stats` tests, ported to
    // drive `DerivedStats::compute` and assert on `derived.caster.*`.
    // -----------------------------------------------------------------

    #[test]
    fn no_inventory_leaves_the_caster_fold_at_identity() {
        let caster = compute_for(None, None).caster;
        assert_eq!(caster.spell_power, 1.0);
        assert_eq!(caster.heal_power, 1.0);
        assert_eq!(caster.energy_regen_modifier, 1.0);
    }

    #[test]
    fn caster_staff_raises_the_three_caster_channels() {
        let inv = inventory_with_mainhand(tool_item(
            tool::ToolKind::Staff,
            tool::WeaponRole::Caster,
            caster_staff_stats(),
        ));
        let caster = compute_for(Some(&inv), None).caster;

        assert!((caster.spell_power - 1.5).abs() < 1e-5);
        assert!((caster.heal_power - 1.75).abs() < 1e-5);
        assert!((caster.energy_regen_modifier - 1.25).abs() < 1e-5);
    }

    #[test]
    fn martial_role_weapon_contributes_nothing() {
        let inv = inventory_with_mainhand(tool_item(
            tool::ToolKind::Staff,
            tool::WeaponRole::Martial,
            caster_staff_stats(),
        ));
        let caster = compute_for(Some(&inv), None).caster;

        assert_eq!(caster.spell_power, 1.0);
        assert_eq!(caster.heal_power, 1.0);
        assert_eq!(caster.energy_regen_modifier, 1.0);
    }

    #[test]
    fn every_caster_implement_kind_contributes_not_just_staff_and_sceptre() {
        for kind in [
            tool::ToolKind::Staff,
            tool::ToolKind::Sceptre,
            tool::ToolKind::Tome,
            tool::ToolKind::HolySymbol,
            tool::ToolKind::Focus,
        ] {
            let inv = inventory_with_mainhand(tool_item(
                kind,
                tool::WeaponRole::Caster,
                caster_staff_stats(),
            ));
            let caster = compute_for(Some(&inv), None).caster;

            assert!(
                (caster.spell_power - 1.5).abs() < 1e-5,
                "{kind:?} caster implement should raise spell_power"
            );
        }
    }

    #[test]
    fn unattuned_requires_attunement_item_contributes_nothing() {
        let inv = inventory_with_mainhand(attunement_required_tool_item(
            tool::ToolKind::Staff,
            tool::WeaponRole::Caster,
            caster_staff_stats(),
        ));
        let caster = compute_for(Some(&inv), None).caster;

        assert_eq!(caster.spell_power, 1.0, "unattuned item must stay inert");
    }

    #[test]
    fn attuning_the_slot_activates_the_contribution() {
        let inv = inventory_with_mainhand(attunement_required_tool_item(
            tool::ToolKind::Staff,
            tool::WeaponRole::Caster,
            caster_staff_stats(),
        ));
        let attuned = AttunedItems(vec![EquipSlot::ActiveMainhand]);
        let caster = compute_for(Some(&inv), Some(&attuned)).caster;

        assert!((caster.spell_power - 1.5).abs() < 1e-5);
    }

    #[test]
    fn unequipping_removes_the_contribution_on_the_next_rebuild() {
        let inv = inventory_with_mainhand(tool_item(
            tool::ToolKind::Staff,
            tool::WeaponRole::Caster,
            caster_staff_stats(),
        ));
        assert!((compute_for(Some(&inv), None).caster.spell_power - 1.5).abs() < 1e-5);

        // A later rebuild recomputes from scratch rather than mutating the
        // previous value, so an unequipped weapon simply stops contributing.
        let empty = empty_inventory();
        assert_eq!(
            compute_for(Some(&empty), None).caster.spell_power,
            1.0,
            "unequipped gear must not linger"
        );
    }

    #[test]
    fn repeated_rebuilds_with_the_same_gear_do_not_compound() {
        let inv = inventory_with_mainhand(tool_item(
            tool::ToolKind::Staff,
            tool::WeaponRole::Caster,
            caster_staff_stats(),
        ));
        let first = compute_for(Some(&inv), None).caster;
        let second = compute_for(Some(&inv), None).caster;

        assert_eq!(first, second);
        assert!((second.spell_power - 1.5).abs() < 1e-5);
    }

    #[test]
    fn a_weaponless_pool_caster_still_benefits_from_an_equipped_caster_implement() {
        // The fold walks every equipped item rather than an "ability hand", so
        // a caster implement in the OFFHAND still raises spell_power.
        let inv = Inventory::with_loadout_humanoid(
            LoadoutBuilder::empty()
                .active_offhand(Some(item_from_kind(ItemKind::Tool(tool::Tool::new(
                    tool::ToolKind::Focus,
                    tool::Hands::One,
                    Some(tool::WeaponRole::Caster),
                    caster_staff_stats(),
                )))))
                .build(),
        );
        let caster = compute_for(Some(&inv), None).caster;

        assert!((caster.spell_power - 1.5).abs() < 1e-5);
    }

    #[test]
    fn caster_gear_cooldown_reduction_folds_into_the_cooldown_channel() {
        let inv = inventory_with_mainhand(tool_item(
            tool::ToolKind::Staff,
            tool::WeaponRole::Caster,
            tool::Stats {
                cooldown_reduction: 0.6,
                ..caster_staff_stats()
            },
        ));
        let caster = compute_for(Some(&inv), None).caster;

        assert!((caster.cooldown_reduction_modifier - 0.6).abs() < 1e-5);
    }

    #[test]
    fn a_martial_weapon_never_reduces_cooldowns() {
        let inv = inventory_with_mainhand(tool_item(
            tool::ToolKind::Staff,
            tool::WeaponRole::Martial,
            tool::Stats {
                cooldown_reduction: 0.2,
                ..caster_staff_stats()
            },
        ));
        let caster = compute_for(Some(&inv), None).caster;

        assert_eq!(caster.cooldown_reduction_modifier, 1.0);
    }

    #[test]
    fn an_unkeyed_caster_item_still_feeds_the_flat_spell_power_channel_only() {
        let inv = inventory_with_mainhand(tool_item(
            tool::ToolKind::Tome,
            tool::WeaponRole::Caster,
            caster_staff_stats(),
        ));
        let caster = compute_for(Some(&inv), None).caster;

        assert!((caster.spell_power - 1.5).abs() < 1e-5);
        assert_eq!(
            caster.spell_power_by_source,
            [1.0; MagicSource::NUM_SOURCES]
        );
    }

    #[test]
    fn a_source_keyed_caster_item_boosts_only_its_own_source() {
        let inv = inventory_with_mainhand(item_from_kind(ItemKind::Tool(
            tool::Tool::new(
                tool::ToolKind::Tome,
                tool::Hands::Two,
                Some(tool::WeaponRole::Caster),
                caster_staff_stats(),
            )
            .with_spell_power_source(MagicSource::Divine),
        )));
        let caster = compute_for(Some(&inv), None).caster;

        // The keyed contribution lands ONLY in the Divine slot...
        assert!((caster.spell_power_by_source[MagicSource::Divine.index()] - 1.5).abs() < 1e-5);
        for source in [
            MagicSource::Arcane,
            MagicSource::Primordial,
            MagicSource::Psionic,
            MagicSource::Ki,
        ] {
            assert_eq!(caster.spell_power_by_source[source.index()], 1.0);
        }
        // ...and NOT the flat channel, which stays at identity.
        assert_eq!(caster.spell_power, 1.0);
    }

    /// The caster fold must produce exactly what `apply_gear_caster_stats`
    /// writes onto a freshly-reset `Stats` — the one place the two
    /// implementations can be compared directly.
    #[test]
    fn compute_equals_legacy_caster_fold() {
        use crate::comp::Stats;

        let inv = inventory_with_mainhand(tool_item(
            tool::ToolKind::Staff,
            tool::WeaponRole::Caster,
            caster_staff_stats(),
        ));
        let caster = compute_for(Some(&inv), None).caster;

        let mut stats = Stats::empty(test_body());
        combat::apply_gear_caster_stats(&mut stats, Some(&inv), None);

        assert_eq!(caster.spell_power, stats.spell_power);
        assert_eq!(caster.spell_power_by_source, stats.spell_power_by_source);
        assert_eq!(caster.heal_power, stats.heal_power);
        assert_eq!(caster.energy_regen_modifier, stats.energy_regen_modifier);
        assert_eq!(
            caster.cooldown_reduction_modifier,
            stats.cooldown_reduction_modifier
        );
    }
}
