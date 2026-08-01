use crate::{
    combat::{self, CombatEffect, DamageKind, Knockback, ScalingKind},
    comp::{
        self, Body, CharacterState, Combo, LightEmitter, StateUpdate, aura, beam,
        buff::{self, BuffKind, Buffs},
        character_state::AttackFilters,
        inventory::{
            Inventory,
            item::{
                ItemDefinitionIdOwned, ItemKind, Tool,
                tool::{
                    AbilityItem, AbilityKind, AbilityMap, AbilitySpec, ContextualIndex, Stats,
                    ToolKind,
                },
            },
            slot::EquipSlot,
        },
        item::Reagent,
        melee::{CustomCombo, MeleeConstructor, MeleeConstructorKind},
        projectile::ProjectileConstructor,
        skillset::{
            SkillSet,
            skills::{self, SKILL_MODIFIERS, Skill},
        },
    },
    explosion::{ColorPreset, TerrainReplacementPreset},
    match_some,
    resources::{Secs, Time},
    states::{
        behavior::JoinData,
        sprite_summon::SpriteSummonAnchor,
        utils::{
            AbilityInfo, ComboConsumption, MovementModifier, OrientationModifier, ProjectileSpread,
            StageSection,
        },
        *,
    },
    terrain::SpriteKind,
};
use hashbrown::HashMap;
use serde::{Deserialize, Serialize};
use specs::{Component, DerefFlaggedStorage};
use std::{borrow::Cow, time::Duration};

pub const BASE_ABILITY_LIMIT: usize = 5;

// NOTE: different AbilitySpec on same ToolKind share the same key
/// Descriptor to pick the right (auxiliary) ability set
pub type AuxiliaryKey = (Option<ToolKind>, Option<ToolKind>);

// TODO: Potentially look into storing previous ability sets for weapon
// combinations and automatically reverting back to them on switching to that
// set of weapons. Consider after UI is set up and people weigh in on memory
// considerations.
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct ActiveAbilities {
    pub guard: GuardAbility,
    pub primary: PrimaryAbility,
    pub secondary: SecondaryAbility,
    pub movement: MovementAbility,
    pub limit: Option<usize>,
    pub auxiliary_sets: HashMap<AuxiliaryKey, Vec<AuxiliaryAbility>>,
}

impl Component for ActiveAbilities {
    type Storage = DerefFlaggedStorage<Self, specs::VecStorage<Self>>;
}

impl Default for ActiveAbilities {
    fn default() -> Self {
        Self {
            guard: GuardAbility::Tool,
            primary: PrimaryAbility::Tool,
            secondary: SecondaryAbility::Tool,
            movement: MovementAbility::Species,
            limit: None,
            auxiliary_sets: HashMap::new(),
        }
    }
}

/// Per-ability cooldowns, keyed by ability id (the RON asset path, or the
/// pool key for innate abilities). Stores the absolute game `Time` at which
/// the ability is ready again; expired entries are pruned opportunistically
/// on `set`, so no tick system is needed (magic-abilities spec §8). Not
/// persisted across logout (accepted v1 exploit surface, spec Open Q #3).
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct AbilityCooldowns(pub HashMap<String, Time>);

impl AbilityCooldowns {
    pub fn is_ready(&self, ability_id: &str, now: Time) -> bool {
        self.0
            .get(ability_id)
            .is_none_or(|ready_at| now.0 >= ready_at.0)
    }

    pub fn ready_at(&self, ability_id: &str) -> Option<Time> { self.0.get(ability_id).copied() }

    pub fn set(&mut self, ability_id: &str, now: Time, cooldown_secs: f32) {
        self.0.retain(|_, ready_at| ready_at.0 > now.0);
        self.0.insert(
            ability_id.to_string(),
            Time(now.0 + f64::from(cooldown_secs)),
        );
    }
}

impl Component for AbilityCooldowns {
    type Storage = DerefFlaggedStorage<Self, specs::DenseVecStorage<Self>>;
}

/// Ability-set keys (manifest `Custom(...)` entries) granted to a character
/// independent of equipment: racial innates and class signature abilities
/// (magic-abilities spec §3 Path B). Indexed by `AuxiliaryAbility::Innate(i)`.
/// Each key's set `primary` is the granted ability; the key itself doubles as
/// the frontend ability id (icon/i18n key), like Contextualized pseudo_ids.
///
/// ORDERING CONTRACT (review M1): persisted hotbar slots store
/// `Innate:index:N` positions into this Vec, so its order must be STABLE and
/// append-only for a given character: producers must emit class abilities
/// first (spec order), then racial innates, never reordering existing
/// entries.
///
/// Canonical order under multiclass:
///
/// ```text
/// [primary class keys] [racial innate] [primary spells]
///                      [secondary class keys] [secondary spells]
/// ```
///
/// Deduplicated by key, so a spell both held classes can cast appears exactly
/// once — at the primary's position, but with BOTH grantor classes recorded in
/// its gate, so either class's level can unlock it. A single-class character is
/// `[P keys][innate][P spells]`; granting a second class appends
/// `[S keys][S spells]` and shifts nothing that already existed — granting a
/// second class to an existing single-class character must never shift the
/// racial innate's index, or every persisted `Innate:index:N` hotbar slot
/// silently re-points to something else on relog. Any future producer
/// appending to this Vec must append after *all* of the above, in whatever
/// order is agreed centrally — never insert into the middle.
///
/// Every spell of every held class is emitted whether or not its class-level
/// band has been reached, exactly like the always-emitted class keys above:
/// the gate lives in [`Self::spell_gates`], never in whether the key is
/// present, so levelling up can never shift an index either.
///
/// Revisit key-based persistence before spell content grows large if this
/// contract proves too fragile.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct AbilityPool {
    pub abilities: Vec<String>,
    /// Parallel to [`Self::abilities`] and ALWAYS the same length:
    /// `Some(gate)` for a spell key, `None` for class-signature and
    /// racial-innate keys (those are gated by `Skill` inside the ability
    /// manifest instead). Kept as a parallel Vec rather than folded into
    /// `abilities` so the index contract documented above, the wire format,
    /// and every existing `abilities` reader stay exactly as they are.
    #[serde(default)]
    pub spell_gates: Vec<Option<SpellGate>>,
}

/// The class-level requirement a spell key in an [`AbilityPool`] carries.
/// Baked in when the pool is built; evaluated live against the character's
/// current `CharacterClass` + level, so it never goes stale and needs no
/// invalidation when the character levels up or multiclasses.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SpellGate {
    /// Every class the character HOLDS that lists this spell. A spell unlocks
    /// as soon as ANY of them has reached the band, so a level-59 Cleric /
    /// level-1 Mage casts a Cleric+Mage spell off the Cleric side. At most two
    /// entries: `CharacterClass` holds at most two classes by design, and the
    /// pool records only the held ones (a spell's compendium `classes` list
    /// may be longer, but the classes the character does not hold could never
    /// unlock it anyway).
    ///
    /// Private so the "at most two, no duplicates" invariant can only be
    /// established through [`Self::new`] / [`Self::add_class`].
    classes: [Option<crate::comp::ClassKind>; 2],
    /// The spell's own level; 0 = cantrip.
    pub spell_level: u8,
}

impl SpellGate {
    /// A gate granted by a single class. Merge further grantors in with
    /// [`Self::add_class`].
    pub fn new(class: crate::comp::ClassKind, spell_level: u8) -> Self {
        Self {
            classes: [Some(class), None],
            spell_level,
        }
    }

    /// Record another held class that also grants this spell. A no-op when the
    /// class is already recorded, or when both slots are taken — which cannot
    /// happen for a real pool, since `CharacterClass` holds at most two
    /// classes.
    pub fn add_class(&mut self, class: crate::comp::ClassKind) {
        if self.classes.contains(&Some(class)) {
            return;
        }
        if let Some(slot) = self.classes.iter_mut().find(|slot| slot.is_none()) {
            *slot = Some(class);
        }
    }

    /// The classes that grant this spell, in the order they were recorded
    /// (primary first). Never empty.
    pub fn classes(&self) -> impl Iterator<Item = crate::comp::ClassKind> + '_ {
        self.classes.iter().copied().flatten()
    }

    /// Does `class` grant this spell?
    pub fn granted_by(&self, class: crate::comp::ClassKind) -> bool {
        self.classes.contains(&Some(class))
    }

    /// The class level any grantor class must reach to unlock this spell.
    /// Floored at 1 because class levels start at 1 — a cantrip is available
    /// from the first class level, not from a level 0 that cannot exist.
    pub fn required_class_level(&self) -> u16 {
        (crate::comp::spell::CLASS_LEVELS_PER_SPELL_LEVEL * u16::from(self.spell_level)).max(1)
    }

    /// `(class, its own level)` for the held grantor class that will unlock
    /// this spell soonest — the one already at the highest class level, since
    /// every grantor needs the same [`Self::required_class_level`]. `None`
    /// when the character holds none of the grantor classes (or has no
    /// `CharacterClass` at all).
    ///
    /// The UI renders "Requires &lt;class&gt; level N" off this, so a spell two
    /// held classes grant names the one the player will actually reach first
    /// rather than an arbitrary side.
    pub fn nearest_grantor(
        &self,
        class: Option<&crate::comp::CharacterClass>,
        character_level: u16,
    ) -> Option<(crate::comp::ClassKind, u16)> {
        class?
            .class_levels(character_level)
            .filter(|(class, _, _)| self.granted_by(*class))
            .map(|(class, class_level, _)| (class, class_level))
            .max_by_key(|(_, class_level)| *class_level)
    }

    /// `true` when ANY class the character holds that grants this spell has
    /// reached the band unlocking [`Self::spell_level`]. Cantrips pass from
    /// class level 1.
    ///
    /// Gates off each CLASS's own level via `CharacterClass::class_levels`,
    /// never `character_level` directly: a Warrior 40 / Warlock 20 must be
    /// capped at the Warlock's spell level 3, not the character's 60.
    ///
    /// Fails CLOSED: a character that holds none of the grantor classes — or
    /// whose `CharacterClass` is missing entirely, e.g. an NPC — gets `false`.
    pub fn is_unlocked(
        &self,
        class: Option<&crate::comp::CharacterClass>,
        character_level: u16,
    ) -> bool {
        self.nearest_grantor(class, character_level)
            .is_some_and(|(_, class_level)| {
                u16::from(self.spell_level) <= crate::comp::spell::spell_level_unlocked(class_level)
            })
    }
}

impl Component for AbilityPool {
    type Storage = DerefFlaggedStorage<Self, specs::DenseVecStorage<Self>>;
}

impl AbilityPool {
    /// The racial-innate pool granted to a character at load by its body's
    /// species (magic-abilities plan Task 14). Non-humanoids get an empty pool.
    /// Kept here (not inline in the upstream-owned `initialize_character_data`)
    /// to shrink the upstream-merge conflict surface.
    pub fn for_body(body: &crate::comp::Body) -> Self {
        use crate::comp::Body;
        let key = match body {
            Body::Humanoid(humanoid) => Some(Self::innate_set_key(humanoid.species)),
            _ => None,
        };
        let abilities: Vec<String> = key.into_iter().map(String::from).collect();
        Self {
            // A body-only pool never contains spells, but the parallel array
            // must still be exactly as long as `abilities`.
            spell_gates: vec![None; abilities.len()],
            abilities,
        }
    }

    /// Full ability pool for a player character: primary class active-ability
    /// keys FIRST (spec order, stable indices for persisted hotbar slots),
    /// then the racial innate key, then the primary's spells, then — if the
    /// character is multiclass — the secondary class's keys and spells last
    /// (see the ordering contract on [`Self::abilities`]'s doc comment for
    /// why the secondary goes at the very end rather than after the primary).
    ///
    /// ALL class-ability keys and ALL spells of every held class are emitted
    /// regardless of whether the player has unlocked them yet. The manifest
    /// `Simple(Some(skill), …)` gate makes an un-unlocked class key resolve to
    /// nothing at use-time, and a spell's class-level band is checked through
    /// [`Self::is_unlocked`] — stable pool indices are thereby guaranteed even
    /// for locked entries.
    pub fn for_character(
        body: &crate::comp::Body,
        character_class: &crate::comp::CharacterClass,
    ) -> Self {
        let mut abilities: Vec<String> = Vec::new();
        let mut spell_gates: Vec<Option<SpellGate>> = Vec::new();

        // Appends a key unless it is already present, keeping the two arrays
        // exactly parallel. Deduplication matters under multiclass, where a
        // spell can be listed by both held classes: the key is emitted once,
        // at the position the FIRST grantor put it (the ordering contract is
        // append-only), but the second grantor is MERGED INTO the existing
        // gate — a spell both held classes grant must unlock as soon as
        // either of them reaches the band, not only the primary.
        fn push_key(
            abilities: &mut Vec<String>,
            spell_gates: &mut Vec<Option<SpellGate>>,
            key: String,
            gate: Option<SpellGate>,
        ) {
            if let Some(existing) = abilities.iter().position(|existing| *existing == key) {
                if let (Some(existing_gate), Some(incoming)) =
                    (spell_gates[existing].as_mut(), gate)
                {
                    for class in incoming.classes() {
                        existing_gate.add_class(class);
                    }
                }
                return;
            }
            abilities.push(key);
            spell_gates.push(gate);
        }

        // 1. Primary's class ability keys.
        for key in Self::class_ability_keys(character_class.primary) {
            push_key(&mut abilities, &mut spell_gates, key.to_string(), None);
        }
        // 2. The racial innate (delegated to `for_body` so the species->key logic lives
        //    in exactly one place). Its index MUST stay put across a multiclass grant.
        for key in Self::for_body(body).abilities {
            push_key(&mut abilities, &mut spell_gates, key, None);
        }
        // 3. Primary's spells, then 4. the secondary's class keys, then
        //    5. the secondary's spells.
        use crate::assets::AssetExt;
        let compendium =
            crate::comp::spell::SpellCompendium::load_expect("common.spells.compendium");
        let compendium = compendium.read();
        let push_spells = |abilities: &mut Vec<String>,
                           spell_gates: &mut Vec<Option<SpellGate>>,
                           class: crate::comp::ClassKind| {
            for spell in compendium.spells_for_class(class) {
                push_key(
                    abilities,
                    spell_gates,
                    spell.pool_key().to_string(),
                    Some(SpellGate::new(class, spell.level)),
                );
            }
        };
        push_spells(&mut abilities, &mut spell_gates, character_class.primary);
        if let Some(secondary) = character_class.secondary {
            for key in Self::class_ability_keys(secondary) {
                push_key(&mut abilities, &mut spell_gates, key.to_string(), None);
            }
            push_spells(&mut abilities, &mut spell_gates, secondary);
        }

        debug_assert_eq!(abilities.len(), spell_gates.len());
        Self {
            abilities,
            spell_gates,
        }
    }

    /// `true` if index `i` may be used right now. Non-spell entries and
    /// out-of-range indices answer `true`, preserving existing behaviour for
    /// everything that is not a spell (weapon/class/racial gating is unchanged
    /// and lives elsewhere).
    pub fn is_unlocked(
        &self,
        index: usize,
        class: Option<&crate::comp::CharacterClass>,
        character_level: u16,
    ) -> bool {
        match self.spell_gates.get(index) {
            Some(Some(gate)) => gate.is_unlocked(class, character_level),
            _ => true,
        }
    }

    /// `Some(gate)` iff index `index` is a spell.
    pub fn spell_gate(&self, index: usize) -> Option<&SpellGate> {
        self.spell_gates.get(index).and_then(Option::as_ref)
    }

    /// Manifest `Custom(...)` keys for a class's active abilities (BL-06
    /// P2a/P2b). Signature first, capstone second — indices are stable
    /// across PRs. Capstone stubs exist as RONs but are gated on skills
    /// nobody has yet; they resolve to the stub (inert) until P2b populates
    /// them.
    fn class_ability_keys(class: crate::comp::ClassKind) -> &'static [&'static str] {
        use crate::comp::ClassKind;
        match class {
            ClassKind::Warrior => &["class.warrior.rally", "class.warrior.onslaught"],
            ClassKind::Mage => &["class.mage.arcanesurge", "class.mage.arcanemastery"],
            ClassKind::Cleric => &["class.cleric.mendinglight", "class.cleric.radiantchannel"],
            ClassKind::Rogue => &["class.rogue.ambush", "class.rogue.vanish"],
            _ => &[],
        }
    }

    /// Manifest `Custom(...)` set key for a humanoid species' racial innate.
    /// Exhaustive on purpose: a new species is a compile error until assigned.
    pub(crate) fn innate_set_key(species: crate::comp::humanoid::Species) -> &'static str {
        use crate::comp::humanoid::Species;
        match species {
            Species::Human => "innate.human",
            Species::Elf => "innate.elf",
            Species::Dwarf => "innate.dwarf",
            Species::Orc => "innate.orc",
            Species::Danari => "innate.danari",
            Species::Draugr => "innate.draugr",
        }
    }
}

/// Xindeler: may this character legitimately *bind* `ability` to one of its
/// auxiliary slots right now?
///
/// Extracted from the `ChangeAbilityEvent` handler so the rule is unit-testable
/// on its own. Only spell keys are judged: an `Innate` index carrying a
/// [`SpellGate`] must have its class-level band reached, and an entity with no
/// [`AbilityPool`] at all has no innate abilities to bind, so it is refused.
/// Every other binding — weapon, glider, empty, and gate-free innate keys —
/// answers `true`, preserving the pre-existing (unvalidated) behaviour rather
/// than silently taking on the whole client-trust problem here.
///
/// The authoritative check still lives at
/// [`ActiveAbilities::activate_ability`]; this one only keeps the action bar
/// honest.
pub fn may_bind_ability(
    ability_pool: Option<&AbilityPool>,
    character_class: Option<&crate::comp::CharacterClass>,
    character_level: u16,
    ability: AuxiliaryAbility,
) -> bool {
    match ability {
        AuxiliaryAbility::Innate(index) => ability_pool
            .is_some_and(|pool| pool.is_unlocked(index, character_class, character_level)),
        AuxiliaryAbility::MainWeapon(_)
        | AuxiliaryAbility::OffWeapon(_)
        | AuxiliaryAbility::Glider(_)
        | AuxiliaryAbility::Empty => true,
    }
}

// make it pub, for UI stuff, if you want
enum AbilitySource {
    Weapons,
    Glider,
}

impl AbilitySource {
    // Get all needed data here and pick the right ability source
    //
    // make it pub, for UI stuff, if you want
    fn determine(char_state: Option<&CharacterState>) -> Self {
        if char_state.is_some_and(|c| c.is_glide_wielded()) {
            Self::Glider
        } else {
            Self::Weapons
        }
    }
}

impl ActiveAbilities {
    pub fn from_auxiliary(
        auxiliary_sets: HashMap<AuxiliaryKey, Vec<AuxiliaryAbility>>,
        limit: Option<usize>,
    ) -> Self {
        // Discard any sets that exceed the limit
        ActiveAbilities {
            auxiliary_sets: auxiliary_sets
                .into_iter()
                .filter(|(_, set)| limit.is_none_or(|limit| set.len() == limit))
                .collect(),
            limit,
            ..Self::default()
        }
    }

    pub fn default_limited(limit: usize) -> Self {
        ActiveAbilities {
            limit: Some(limit),
            ..Default::default()
        }
    }

    pub fn change_ability(
        &mut self,
        slot: usize,
        auxiliary_key: AuxiliaryKey,
        new_ability: AuxiliaryAbility,
        inventory: Option<&Inventory>,
        skill_set: Option<&SkillSet>,
    ) {
        let auxiliary_set = self
            .auxiliary_sets
            .entry(auxiliary_key)
            .or_insert(Self::default_ability_set(inventory, skill_set, self.limit));
        if let Some(ability) = auxiliary_set.get_mut(slot) {
            *ability = new_ability;
        }
    }

    pub fn active_auxiliary_key(inv: Option<&Inventory>) -> AuxiliaryKey {
        let tool_kind = |slot| {
            inv.and_then(|inv| inv.equipped(slot))
                .and_then(|item| match_some!(&*item.kind(), ItemKind::Tool(tool) => tool.kind))
        };

        (
            tool_kind(EquipSlot::ActiveMainhand),
            tool_kind(EquipSlot::ActiveOffhand),
        )
    }

    pub fn auxiliary_set(
        &self,
        inv: Option<&Inventory>,
        skill_set: Option<&SkillSet>,
    ) -> Cow<'_, Vec<AuxiliaryAbility>> {
        let aux_key = Self::active_auxiliary_key(inv);

        self.auxiliary_sets
            .get(&aux_key)
            .map(Cow::Borrowed)
            .unwrap_or_else(|| Cow::Owned(Self::default_ability_set(inv, skill_set, self.limit)))
    }

    pub fn get_ability(
        &self,
        input: AbilityInput,
        inventory: Option<&Inventory>,
        skill_set: Option<&SkillSet>,
        stats: Option<&comp::Stats>,
    ) -> Ability {
        match input {
            AbilityInput::Guard => self.guard.into(),
            AbilityInput::Primary => self.primary.into(),
            AbilityInput::Secondary => self.secondary.into(),
            AbilityInput::Movement => self.movement.into(),
            AbilityInput::Auxiliary(index) => {
                if stats.is_some_and(|s| s.disable_auxiliary_abilities) {
                    Ability::Empty
                } else {
                    self.auxiliary_set(inventory, skill_set)
                        .get(index)
                        .copied()
                        .map(|a| a.into())
                        .unwrap_or(Ability::Empty)
                }
            },
        }
    }

    /// Returns the CharacterAbility from an ability input, and also whether the
    /// ability was from a weapon wielded in the offhand
    pub fn activate_ability(
        &self,
        input: AbilityInput,
        inv: Option<&Inventory>,
        attuned: Option<&comp::AttunedItems>,
        skill_set: &SkillSet,
        body: Option<&Body>,
        char_state: Option<&CharacterState>,
        stance: Option<&Stance>,
        combo: Option<&Combo>,
        stats: Option<&comp::Stats>,
        buffs: Option<&Buffs>,
        ability_pool: Option<&AbilityPool>,
        // Xindeler: the caster's class(es), so a spell key in `ability_pool`
        // can be checked against the class-level band that unlocks it. `None`
        // means "no class", which refuses every gated key — correct for NPCs,
        // whose pools hold no spells anyway.
        character_class: Option<&crate::comp::CharacterClass>,
        ability_map: &AbilityMap,
        // bool is from_offhand
    ) -> Option<(CharacterAbility, bool, SpecifiedAbility)> {
        let ability = self.get_ability(input, inv, Some(skill_set), stats);

        // ENG-D2c: a weapon that RequiresAttunement grants no abilities until its
        // slot is attuned (the item is inert).
        let ability_set = |equip_slot| {
            inv.and_then(|inv| inv.equipped(equip_slot))
                .filter(|item| {
                    comp::item_effects_active(equip_slot, item.requires_attunement(), attuned)
                })
                .and_then(|i| i.item_config().map(|c| &c.abilities))
        };

        let scale_ability = |ability: CharacterAbility, equip_slot| {
            let tool_kind = inv
                .and_then(|inv| inv.equipped(equip_slot))
                .and_then(|item| match_some!(&*item.kind(), ItemKind::Tool(tool) => tool.kind));
            ability.adjusted_by_skills(skill_set, tool_kind)
        };

        let spec_ability = |context_index| SpecifiedAbility {
            ability,
            context_index,
        };

        // This function is an attempt to generalize ability handling
        let inst_ability = |slot: EquipSlot, offhand: bool| {
            ability_set(slot).and_then(|abilities| {
                // We use AbilityInput here as an object to match on, which
                // roughly corresponds to all needed data we need to know about
                // ability.
                use AbilityInput as I;

                // Also we don't provide `ability`, nor `ability_input` as an
                // argument to the closure, and that wins us a bit of code
                // duplication we would need to do otherwise, but it's
                // important that we can and do re-create all needed Ability
                // information here to make decisions.
                //
                // For example, we should't take `input` argument provided to
                // activate_abilities, because in case of Auxiliary abilities,
                // it has wrong index.
                //
                // We could alternatively just take `ability`, but it works too.
                let dispatched = match ability.try_ability_set_key()? {
                    I::Guard => abilities.guard(Some(skill_set), stance, inv, combo, buffs),
                    I::Primary => abilities.primary(Some(skill_set), stance, inv, combo, buffs),
                    I::Secondary => abilities.secondary(Some(skill_set), stance, inv, combo, buffs),
                    I::Auxiliary(index) => {
                        abilities.auxiliary(index, Some(skill_set), stance, inv, combo, buffs)
                    },
                    I::Movement => return None,
                };

                dispatched
                    .map(|(a, i)| (a.ability.clone(), i))
                    .map(|(a, i)| (scale_ability(a, slot), offhand, spec_ability(i)))
            })
        };

        let source = AbilitySource::determine(char_state);

        match ability {
            Ability::ToolGuard => match source {
                AbilitySource::Weapons => {
                    let equip_slot = combat::get_equip_slot_by_block_priority(inv);
                    inst_ability(equip_slot, matches!(equip_slot, EquipSlot::ActiveOffhand))
                },
                AbilitySource::Glider => None,
            },
            Ability::ToolPrimary => match source {
                AbilitySource::Weapons => inst_ability(EquipSlot::ActiveMainhand, false),
                AbilitySource::Glider => inst_ability(EquipSlot::Glider, false),
            },
            Ability::ToolSecondary => match source {
                AbilitySource::Weapons => inst_ability(EquipSlot::ActiveOffhand, true)
                    .or_else(|| inst_ability(EquipSlot::ActiveMainhand, false)),
                AbilitySource::Glider => inst_ability(EquipSlot::Glider, false),
            },
            Ability::MainWeaponAux(_) => inst_ability(EquipSlot::ActiveMainhand, false),
            Ability::OffWeaponAux(_) => inst_ability(EquipSlot::ActiveOffhand, true),
            Ability::GliderAux(_) => inst_ability(EquipSlot::Glider, false),
            Ability::InnateAux(index) => ability_pool
                // Xindeler: a spell key whose class-level band has not been
                // reached yet is not castable. Non-spell keys keep their
                // existing behaviour (`is_unlocked` answers `true` for them),
                // so this is a no-op for class and racial innates.
                .filter(|pool| {
                    pool.is_unlocked(index, character_class, skill_set.character_level())
                })
                .and_then(|pool| pool.abilities.get(index))
                .and_then(|key| {
                    ability_map
                        .get_ability_set(&AbilitySpec::Custom(key.clone()))
                        .and_then(|set| set.primary(Some(skill_set), stance, inv, combo, buffs))
                        .map(|(item, i)| {
                            (
                                item.ability
                                    .clone()
                                    .adjusted_by_skills(skill_set, None)
                                    .adjusted_by_class_synergy(skill_set, key),
                                false,
                                spec_ability(i),
                            )
                        })
                }),
            Ability::Empty => None,
            Ability::SpeciesMovement => matches!(body, Some(Body::Humanoid(_)))
                .then(|| CharacterAbility::default_roll(char_state))
                .map(|ability| {
                    (
                        ability.adjusted_by_skills(skill_set, None),
                        false,
                        spec_ability(None),
                    )
                }),
        }
    }

    pub fn iter_available_abilities_on<'a>(
        inv: Option<&'a Inventory>,
        skill_set: Option<&'a SkillSet>,
        equip_slot: EquipSlot,
    ) -> impl Iterator<Item = usize> + 'a {
        inv.and_then(|inv| inv.equipped(equip_slot).and_then(|i| i.item_config()))
            .into_iter()
            .flat_map(|config| &config.abilities.abilities)
            .enumerate()
            .filter_map(move |(i, a)| match a {
                AbilityKind::Simple(skill, _) => skill
                    .is_none_or(|s| skill_set.is_some_and(|ss| ss.has_skill(s)))
                    .then_some(i),
                AbilityKind::Contextualized {
                    pseudo_id: _,
                    abilities,
                } => abilities
                    .iter()
                    .any(|(_contexts, (skill, _))| {
                        skill.is_none_or(|s| skill_set.is_some_and(|ss| ss.has_skill(s)))
                    })
                    .then_some(i),
            })
    }

    /// Weapon, glider, and class/racial innate abilities. Spells are
    /// EXCLUDED — they are listed separately by [`Self::all_available_spells`]
    /// so the UI can present them on their own terms.
    pub fn all_available_abilities(
        inv: Option<&Inventory>,
        skill_set: Option<&SkillSet>,
        ability_pool: Option<&AbilityPool>,
    ) -> Vec<AuxiliaryAbility> {
        let mut ability_buff = vec![];
        // Check if uses combo of two "equal" weapons
        let paired = inv
            .and_then(|inv| {
                let a = inv.equipped(EquipSlot::ActiveMainhand)?;
                let b = inv.equipped(EquipSlot::ActiveOffhand)?;

                if let (ItemKind::Tool(tool_a), ItemKind::Tool(tool_b)) = (&*a.kind(), &*b.kind()) {
                    Some((a.ability_spec(), tool_a.kind, b.ability_spec(), tool_b.kind))
                } else {
                    None
                }
            })
            .is_some_and(|(a_spec, a_kind, b_spec, b_kind)| (a_spec, a_kind) == (b_spec, b_kind));

        // Push main weapon abilities
        Self::iter_available_abilities_on(inv, skill_set, EquipSlot::ActiveMainhand)
            .map(AuxiliaryAbility::MainWeapon)
            .for_each(|a| ability_buff.push(a));

        // Push secondary weapon abilities, if different
        // If equal, just take the first
        if !paired {
            Self::iter_available_abilities_on(inv, skill_set, EquipSlot::ActiveOffhand)
                .map(AuxiliaryAbility::OffWeapon)
                .for_each(|a| ability_buff.push(a));
        }
        // Push glider abilities
        Self::iter_available_abilities_on(inv, skill_set, EquipSlot::Glider)
            .map(AuxiliaryAbility::Glider)
            .for_each(|a| ability_buff.push(a));

        // Push innate (class/racial) abilities. Spell keys are skipped: they
        // are listed by `all_available_spells` instead.
        if let Some(pool) = ability_pool {
            (0..pool.abilities.len())
                .filter(|i| pool.spell_gate(*i).is_none())
                .map(AuxiliaryAbility::Innate)
                .for_each(|a| ability_buff.push(a));
        }

        ability_buff
    }

    /// Every spell key in the pool, unlocked or not, in pool order, paired
    /// with whether it is currently castable. Locked entries are returned
    /// rather than filtered out so the UI can show them greyed with their
    /// requirement instead of hiding what is coming.
    pub fn all_available_spells(
        ability_pool: Option<&AbilityPool>,
        character_class: Option<&crate::comp::CharacterClass>,
        character_level: u16,
    ) -> Vec<(AuxiliaryAbility, bool)> {
        ability_pool
            .into_iter()
            .flat_map(|pool| {
                (0..pool.abilities.len()).filter_map(move |i| {
                    pool.spell_gate(i).map(|gate| {
                        (
                            AuxiliaryAbility::Innate(i),
                            gate.is_unlocked(character_class, character_level),
                        )
                    })
                })
            })
            .collect()
    }

    fn default_ability_set<'a>(
        inv: Option<&'a Inventory>,
        skill_set: Option<&'a SkillSet>,
        limit: Option<usize>,
    ) -> Vec<AuxiliaryAbility> {
        let mut iter = Self::iter_available_abilities_on(inv, skill_set, EquipSlot::ActiveMainhand)
            .map(AuxiliaryAbility::MainWeapon)
            .chain(
                Self::iter_available_abilities_on(inv, skill_set, EquipSlot::ActiveOffhand)
                    .map(AuxiliaryAbility::OffWeapon),
            );

        if let Some(limit) = limit {
            (0..limit)
                .map(|_| iter.next().unwrap_or(AuxiliaryAbility::Empty))
                .collect()
        } else {
            iter.collect()
        }
    }
}

#[derive(Debug, Copy, Clone)]
pub enum AbilityInput {
    Guard,
    Primary,
    Secondary,
    Movement,
    Auxiliary(usize),
}

#[derive(Copy, Clone, Serialize, Deserialize, Debug, PartialEq, Eq)]
pub enum Ability {
    ToolGuard,
    ToolPrimary,
    ToolSecondary,
    SpeciesMovement,
    MainWeaponAux(usize),
    OffWeaponAux(usize),
    GliderAux(usize),
    InnateAux(usize),
    Empty,
    /* For future use
     * ArmorAbility(usize), */
}

impl Ability {
    // Used for generic ability dispatch (inst_ability) in this file
    //
    // It does use AbilityInput to avoid creating just another enum, but it is
    // semantically different.
    fn try_ability_set_key(&self) -> Option<AbilityInput> {
        let input = match self {
            Self::ToolGuard => AbilityInput::Guard,
            Self::ToolPrimary => AbilityInput::Primary,
            Self::ToolSecondary => AbilityInput::Secondary,
            Self::SpeciesMovement => AbilityInput::Movement,
            Self::GliderAux(idx)
            | Self::OffWeaponAux(idx)
            | Self::MainWeaponAux(idx)
            | Self::InnateAux(idx) => AbilityInput::Auxiliary(*idx),
            Self::Empty => return None,
        };

        Some(input)
    }

    pub fn ability_id<'a>(
        self,
        char_state: Option<&CharacterState>,
        inv: Option<&'a Inventory>,
        skill_set: Option<&'a SkillSet>,
        ability_pool: Option<&'a AbilityPool>,
        stance: Option<&Stance>,
        combo: Option<&Combo>,
        buffs: Option<&Buffs>,
    ) -> Option<&'a str> {
        let ability_set = |equip_slot| {
            inv.and_then(|inv| inv.equipped(equip_slot))
                .and_then(|i| i.item_config().map(|c| &c.abilities))
        };

        let contextual_id = |kind: Option<&'a AbilityKind<_>>| -> Option<&'a str> {
            if let Some(AbilityKind::Contextualized {
                pseudo_id,
                abilities: _,
            }) = kind
            {
                Some(pseudo_id.as_str())
            } else {
                None
            }
        };

        let inst_ability = |slot: EquipSlot| {
            ability_set(slot).and_then(|abilities| {
                use AbilityInput as I;

                let dispatched = match self.try_ability_set_key()? {
                    I::Guard => abilities.guard(skill_set, stance, inv, combo, buffs),
                    I::Primary => abilities.primary(skill_set, stance, inv, combo, buffs),
                    I::Secondary => abilities.secondary(skill_set, stance, inv, combo, buffs),
                    I::Auxiliary(index) => {
                        abilities.auxiliary(index, skill_set, stance, inv, combo, buffs)
                    },
                    I::Movement => return None,
                };

                dispatched.map(|(a, _)| a.id.as_str()).or_else(|| {
                    match self.try_ability_set_key()? {
                        I::Guard => abilities
                            .guard
                            .as_ref()
                            .and_then(|g| contextual_id(Some(g))),
                        I::Primary => contextual_id(Some(&abilities.primary)),
                        I::Secondary => contextual_id(Some(&abilities.secondary)),
                        I::Auxiliary(index) => contextual_id(abilities.abilities.get(index)),
                        I::Movement => None,
                    }
                })
            })
        };

        let source = AbilitySource::determine(char_state);
        match source {
            AbilitySource::Glider => match self {
                Ability::ToolGuard => None,
                Ability::ToolPrimary => inst_ability(EquipSlot::Glider),
                Ability::ToolSecondary => inst_ability(EquipSlot::Glider),
                Ability::SpeciesMovement => None, // TODO: Make not None
                Ability::MainWeaponAux(_) => inst_ability(EquipSlot::ActiveMainhand),
                Ability::OffWeaponAux(_) => inst_ability(EquipSlot::ActiveOffhand),
                Ability::GliderAux(_) => inst_ability(EquipSlot::Glider),
                Ability::InnateAux(index) => ability_pool
                    .and_then(|pool| pool.abilities.get(index))
                    .map(|key| key.as_str()),
                Ability::Empty => None,
            },
            AbilitySource::Weapons => match self {
                Ability::ToolGuard => {
                    let equip_slot = combat::get_equip_slot_by_block_priority(inv);
                    inst_ability(equip_slot)
                },
                Ability::ToolPrimary => inst_ability(EquipSlot::ActiveMainhand),
                Ability::ToolSecondary => inst_ability(EquipSlot::ActiveOffhand)
                    .or_else(|| inst_ability(EquipSlot::ActiveMainhand)),
                Ability::SpeciesMovement => None, // TODO: Make not None
                Ability::MainWeaponAux(_) => inst_ability(EquipSlot::ActiveMainhand),
                Ability::OffWeaponAux(_) => inst_ability(EquipSlot::ActiveOffhand),
                Ability::GliderAux(_) => inst_ability(EquipSlot::Glider),
                Ability::InnateAux(index) => ability_pool
                    .and_then(|pool| pool.abilities.get(index))
                    .map(|key| key.as_str()),
                Ability::Empty => None,
            },
        }
    }

    pub fn is_from_wielded(&self) -> bool {
        match self {
            Ability::ToolPrimary
            | Ability::ToolSecondary
            | Ability::MainWeaponAux(_)
            | Ability::GliderAux(_)
            | Ability::OffWeaponAux(_)
            | Ability::ToolGuard => true,
            Ability::InnateAux(_) | Ability::SpeciesMovement | Ability::Empty => false,
        }
    }
}

#[derive(Copy, Clone, Serialize, Deserialize, Debug)]
pub enum GuardAbility {
    Tool,
    Empty,
}

impl From<GuardAbility> for Ability {
    fn from(guard: GuardAbility) -> Self {
        match guard {
            GuardAbility::Tool => Ability::ToolGuard,
            GuardAbility::Empty => Ability::Empty,
        }
    }
}

#[derive(Copy, Clone, Serialize, Deserialize, Debug, PartialEq, Eq)]
pub struct SpecifiedAbility {
    pub ability: Ability,
    pub context_index: Option<ContextualIndex>,
}

impl SpecifiedAbility {
    pub fn ability_id<'a>(
        self,
        char_state: Option<&CharacterState>,
        inv: Option<&'a Inventory>,
        ability_pool: Option<&'a AbilityPool>,
    ) -> Option<&'a str> {
        let ability_set = |equip_slot| {
            inv.and_then(|inv| inv.equipped(equip_slot))
                .and_then(|i| i.item_config().map(|c| &c.abilities))
        };

        fn ability_id(spec_ability: SpecifiedAbility, ability: &AbilityKind<AbilityItem>) -> &str {
            match ability {
                AbilityKind::Simple(_, a) => a.id.as_str(),
                AbilityKind::Contextualized {
                    pseudo_id,
                    abilities,
                } => spec_ability
                    .context_index
                    .and_then(|i| abilities.get(i.0))
                    .map_or(pseudo_id.as_str(), |(_, (_, a))| a.id.as_str()),
            }
        }

        let inst_ability = |slot: EquipSlot| {
            ability_set(slot).and_then(|abilities| {
                use AbilityInput as I;

                let dispatched = match self.ability.try_ability_set_key()? {
                    I::Guard => abilities.guard.as_ref(),
                    I::Primary => Some(&abilities.primary),
                    I::Secondary => Some(&abilities.secondary),
                    I::Auxiliary(index) => abilities.abilities.get(index),
                    I::Movement => return None,
                };
                dispatched.map(|a| ability_id(self, a))
            })
        };

        let source = AbilitySource::determine(char_state);
        match source {
            AbilitySource::Glider => match self.ability {
                Ability::ToolGuard => None,
                Ability::ToolPrimary => inst_ability(EquipSlot::Glider),
                Ability::ToolSecondary => inst_ability(EquipSlot::Glider),
                Ability::SpeciesMovement => None,
                Ability::MainWeaponAux(_) => inst_ability(EquipSlot::ActiveMainhand),
                Ability::OffWeaponAux(_) => inst_ability(EquipSlot::ActiveOffhand),
                Ability::GliderAux(_) => inst_ability(EquipSlot::Glider),
                Ability::InnateAux(index) => ability_pool
                    .and_then(|pool| pool.abilities.get(index))
                    .map(|key| key.as_str()),
                Ability::Empty => None,
            },
            AbilitySource::Weapons => match self.ability {
                Ability::ToolGuard => inst_ability(combat::get_equip_slot_by_block_priority(inv)),
                Ability::ToolPrimary => inst_ability(EquipSlot::ActiveMainhand),
                Ability::ToolSecondary => inst_ability(EquipSlot::ActiveOffhand)
                    .or_else(|| inst_ability(EquipSlot::ActiveMainhand)),
                Ability::SpeciesMovement => None, // TODO: Make not None
                Ability::MainWeaponAux(_) => inst_ability(EquipSlot::ActiveMainhand),
                Ability::OffWeaponAux(_) => inst_ability(EquipSlot::ActiveOffhand),
                Ability::GliderAux(_) => inst_ability(EquipSlot::Glider),
                Ability::InnateAux(index) => ability_pool
                    .and_then(|pool| pool.abilities.get(index))
                    .map(|key| key.as_str()),
                Ability::Empty => None,
            },
        }
    }
}

#[derive(Copy, Clone, Serialize, Deserialize, Debug)]
pub enum PrimaryAbility {
    Tool,
    Empty,
}

impl From<PrimaryAbility> for Ability {
    fn from(primary: PrimaryAbility) -> Self {
        match primary {
            PrimaryAbility::Tool => Ability::ToolPrimary,
            PrimaryAbility::Empty => Ability::Empty,
        }
    }
}

#[derive(Copy, Clone, Serialize, Deserialize, Debug)]
pub enum SecondaryAbility {
    Tool,
    Empty,
}

impl From<SecondaryAbility> for Ability {
    fn from(primary: SecondaryAbility) -> Self {
        match primary {
            SecondaryAbility::Tool => Ability::ToolSecondary,
            SecondaryAbility::Empty => Ability::Empty,
        }
    }
}

#[derive(Copy, Clone, Serialize, Deserialize, Debug)]
pub enum MovementAbility {
    Species,
    Empty,
}

impl From<MovementAbility> for Ability {
    fn from(primary: MovementAbility) -> Self {
        match primary {
            MovementAbility::Species => Ability::SpeciesMovement,
            MovementAbility::Empty => Ability::Empty,
        }
    }
}

#[derive(Copy, Clone, Serialize, Deserialize, Debug, PartialEq, Eq)]
pub enum AuxiliaryAbility {
    MainWeapon(usize),
    OffWeapon(usize),
    Glider(usize),
    Innate(usize),
    Empty,
}

impl From<AuxiliaryAbility> for Ability {
    fn from(primary: AuxiliaryAbility) -> Self {
        match primary {
            AuxiliaryAbility::MainWeapon(i) => Ability::MainWeaponAux(i),
            AuxiliaryAbility::OffWeapon(i) => Ability::OffWeaponAux(i),
            AuxiliaryAbility::Glider(i) => Ability::GliderAux(i),
            AuxiliaryAbility::Innate(i) => Ability::InnateAux(i),
            AuxiliaryAbility::Empty => Ability::Empty,
        }
    }
}

/// A lighter form of character state to pass around as needed for frontend
/// purposes
// Only add to this enum as needed for frontends, not necessary to immediately
// add a variant here when adding a new character state
#[derive(Copy, Clone, Hash, Eq, PartialEq, Debug, Serialize, Deserialize)]
pub enum CharacterAbilityType {
    BasicMelee(StageSection),
    BasicRanged,
    Boost,
    ChargedMelee(StageSection),
    ChargedRanged,
    DashMelee(StageSection),
    BasicBlock,
    ComboMelee2(StageSection),
    FinisherMelee(StageSection),
    DiveMelee(StageSection),
    RiposteMelee(StageSection),
    RapidMelee(StageSection),
    LeapMelee(StageSection),
    LeapShockwave(StageSection),
    Music(StageSection),
    Shockwave,
    BasicBeam,
    RapidRanged,
    BasicAura,
    SelfBuff,
    Other,
}

impl From<&CharacterState> for CharacterAbilityType {
    fn from(state: &CharacterState) -> Self {
        match state {
            CharacterState::BasicMelee(data) => Self::BasicMelee(data.stage_section),
            CharacterState::BasicRanged(_) => Self::BasicRanged,
            CharacterState::Boost(_) => Self::Boost,
            CharacterState::DashMelee(data) => Self::DashMelee(data.stage_section),
            CharacterState::BasicBlock(_) => Self::BasicBlock,
            CharacterState::LeapMelee(data) => Self::LeapMelee(data.stage_section),
            CharacterState::LeapShockwave(data) => Self::LeapShockwave(data.stage_section),
            CharacterState::ComboMelee2(data) => Self::ComboMelee2(data.stage_section),
            CharacterState::FinisherMelee(data) => Self::FinisherMelee(data.stage_section),
            CharacterState::DiveMelee(data) => Self::DiveMelee(data.stage_section),
            CharacterState::RiposteMelee(data) => Self::RiposteMelee(data.stage_section),
            CharacterState::RapidMelee(data) => Self::RapidMelee(data.stage_section),
            CharacterState::ChargedMelee(data) => Self::ChargedMelee(data.stage_section),
            CharacterState::ChargedRanged(_) => Self::ChargedRanged,
            CharacterState::Shockwave(_) => Self::Shockwave,
            CharacterState::BasicBeam(_) => Self::BasicBeam,
            CharacterState::RapidRanged(_) => Self::RapidRanged,
            CharacterState::BasicAura(_) => Self::BasicAura,
            CharacterState::SelfBuff(_) => Self::SelfBuff,
            CharacterState::Music(data) => Self::Music(data.stage_section),
            CharacterState::Idle(_)
            | CharacterState::Crawl
            | CharacterState::Climb(_)
            | CharacterState::Sit
            | CharacterState::Dance
            | CharacterState::Talk(_)
            | CharacterState::Glide(_)
            | CharacterState::GlideWield(_)
            | CharacterState::Stunned(_)
            | CharacterState::Equipping(_)
            | CharacterState::Wielding(_)
            | CharacterState::Roll(_)
            | CharacterState::Blink(_)
            | CharacterState::BasicSummon(_)
            | CharacterState::SpriteSummon(_)
            | CharacterState::UseItem(_)
            | CharacterState::Interact(_)
            | CharacterState::Skate(_)
            | CharacterState::Transform(_)
            | CharacterState::RegrowHead(_)
            | CharacterState::Wallrun(_)
            | CharacterState::StaticAura(_)
            | CharacterState::Throw(_)
            | CharacterState::LeapExplosionShockwave(_)
            | CharacterState::Explosion(_)
            | CharacterState::GroundAoe(_)
            | CharacterState::LeapRanged(_)
            | CharacterState::Simple(_)
            | CharacterState::TelekineticGrip(_)
            | CharacterState::Knock(_) => Self::Other,
        }
    }
}

#[derive(Copy, Clone, Debug, PartialEq, Serialize, Deserialize, Default)]
pub enum Dodgeable {
    #[default]
    Roll,
    Jump,
    No,
}

#[derive(Clone, Copy, PartialEq, Debug, Serialize, Deserialize)]
pub enum Amount {
    PerHead(u32),
    Value(u32),
}

impl Amount {
    pub fn add(&mut self, value: u32) {
        match self {
            Self::PerHead(v) | Self::Value(v) => *v += value,
        }
    }

    pub fn compute(&self, heads: u32) -> u32 {
        match self {
            Amount::PerHead(v) => v * heads,
            Amount::Value(v) => *v,
        }
    }
}

impl Default for Amount {
    fn default() -> Self { Self::Value(1) }
}

#[derive(Clone, PartialEq, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
/// For documentation on individual fields, see the corresponding character
/// state file in 'common/src/states/'
pub enum CharacterAbility {
    BasicMelee {
        energy_cost: f32,
        buildup_duration: f32,
        swing_duration: f32,
        hit_timing: f32,
        recover_duration: f32,
        melee_constructor: MeleeConstructor,
        #[serde(default)]
        movement_modifier: MovementModifier,
        #[serde(default)]
        ori_modifier: OrientationModifier,
        frontend_specifier: Option<basic_melee::FrontendSpecifier>,
        #[serde(default)]
        meta: AbilityMeta,
    },
    BasicRanged {
        energy_cost: f32,
        buildup_duration: f32,
        recover_duration: f32,
        projectile: ProjectileConstructor,
        projectile_body: Body,
        projectile_light: Option<LightEmitter>,
        projectile_speed: f32,
        #[serde(default)]
        vertical_angle_offset: f32,
        #[serde(default)]
        num_projectiles: Amount,
        projectile_spread: Option<ProjectileSpread>,
        #[serde(default)]
        auto_aim: bool,
        #[serde(default)]
        movement_modifier: MovementModifier,
        #[serde(default)]
        ori_modifier: OrientationModifier,
        marker: Option<comp::FrontendMarker>,
        #[serde(default)]
        meta: AbilityMeta,
    },
    RapidRanged {
        #[serde(default)]
        initial_energy: f32,
        #[serde(default)]
        energy_cost: f32,
        buildup_duration: f32,
        shoot_duration: f32,
        recover_duration: f32,
        options: rapid_ranged::Options,
        projectile: ProjectileConstructor,
        projectile_body: Body,
        projectile_light: Option<LightEmitter>,
        projectile_speed: f32,
        specifier: Option<rapid_ranged::FrontendSpecifier>,
        #[serde(default)]
        meta: AbilityMeta,
    },
    Boost {
        movement_duration: f32,
        only_up: bool,
        speed: f32,
        max_exit_velocity: f32,
        #[serde(default)]
        meta: AbilityMeta,
    },
    GlideBoost {
        booster: glide::Boost,
        #[serde(default)]
        meta: AbilityMeta,
    },
    DashMelee {
        energy_cost: f32,
        energy_drain: f32,
        forward_speed: f32,
        buildup_duration: f32,
        charge_duration: f32,
        swing_duration: f32,
        recover_duration: f32,
        melee_constructor: MeleeConstructor,
        ori_modifier: f32,
        auto_charge: bool,
        #[serde(default)]
        charge_through: bool,
        #[serde(default)]
        frontend_specifier: Option<dash_melee::FrontendSpecifier>,
        #[serde(default)]
        meta: AbilityMeta,
    },
    BasicBlock {
        buildup_duration: f32,
        recover_duration: f32,
        max_angle: f32,
        block_strength: f32,
        parry_window: basic_block::ParryWindow,
        energy_cost: f32,
        energy_regen: f32,
        can_hold: bool,
        blocked_attacks: AttackFilters,
        #[serde(default)]
        meta: AbilityMeta,
    },
    Roll {
        energy_cost: f32,
        buildup_duration: f32,
        movement_duration: f32,
        recover_duration: f32,
        roll_strength: f32,
        attack_immunities: AttackFilters,
        was_cancel: bool,
        #[serde(default)]
        meta: AbilityMeta,
    },
    ComboMelee2 {
        strikes: Vec<combo_melee2::Strike<f32>>,
        energy_cost_per_strike: f32,
        specifier: Option<combo_melee2::FrontendSpecifier>,
        #[serde(default)]
        auto_progress: bool,
        #[serde(default)]
        meta: AbilityMeta,
    },
    LeapExplosionShockwave {
        energy_cost: f32,
        buildup_duration: f32,
        movement_duration: f32,
        swing_duration: f32,
        recover_duration: f32,
        forward_leap_strength: f32,
        vertical_leap_strength: f32,
        explosion_damage: f32,
        explosion_poise: f32,
        explosion_knockback: Knockback,
        explosion_radius: f32,
        min_falloff: f32,
        #[serde(default)]
        explosion_dodgeable: Dodgeable,
        #[serde(default)]
        destroy_terrain: Option<(f32, ColorPreset)>,
        #[serde(default)]
        replace_terrain: Option<(f32, TerrainReplacementPreset)>,
        #[serde(default)]
        eye_height: bool,
        #[serde(default)]
        reagent: Option<Reagent>,
        shockwave_damage: f32,
        shockwave_poise: f32,
        shockwave_knockback: Knockback,
        shockwave_angle: f32,
        shockwave_vertical_angle: f32,
        shockwave_speed: f32,
        shockwave_duration: f32,
        #[serde(default)]
        shockwave_dodgeable: Dodgeable,
        #[serde(default)]
        shockwave_damage_effect: Option<CombatEffect>,
        shockwave_damage_kind: DamageKind,
        shockwave_specifier: comp::shockwave::FrontendSpecifier,
        move_efficiency: f32,
        #[serde(default)]
        meta: AbilityMeta,
    },
    LeapMelee {
        energy_cost: f32,
        buildup_duration: f32,
        movement_duration: f32,
        swing_duration: f32,
        recover_duration: f32,
        melee_constructor: MeleeConstructor,
        forward_leap_strength: f32,
        vertical_leap_strength: f32,
        specifier: Option<leap_melee::FrontendSpecifier>,
        #[serde(default)]
        meta: AbilityMeta,
    },
    LeapShockwave {
        energy_cost: f32,
        buildup_duration: f32,
        movement_duration: f32,
        swing_duration: f32,
        recover_duration: f32,
        damage: f32,
        poise_damage: f32,
        knockback: Knockback,
        shockwave_angle: f32,
        shockwave_vertical_angle: f32,
        shockwave_speed: f32,
        shockwave_duration: f32,
        dodgeable: Dodgeable,
        move_efficiency: f32,
        damage_kind: DamageKind,
        specifier: comp::shockwave::FrontendSpecifier,
        damage_effect: Option<CombatEffect>,
        forward_leap_strength: f32,
        vertical_leap_strength: f32,
        #[serde(default)]
        meta: AbilityMeta,
    },
    ChargedMelee {
        energy_cost: f32,
        energy_drain: f32,
        buildup_strike: Option<(f32, MeleeConstructor)>,
        charge_duration: f32,
        swing_duration: f32,
        hit_timing: f32,
        recover_duration: f32,
        melee_constructor: MeleeConstructor,
        specifier: Option<charged_melee::FrontendSpecifier>,
        #[serde(default)]
        custom_combo: CustomCombo,
        #[serde(default)]
        meta: AbilityMeta,
        #[serde(default)]
        movement_modifier: MovementModifier,
        #[serde(default)]
        ori_modifier: OrientationModifier,
    },
    ChargedRanged {
        energy_cost: f32,
        energy_drain: f32,
        idle_drain: f32,
        projectile: ProjectileConstructor,
        buildup_duration: f32,
        charge_duration: f32,
        recover_duration: f32,
        projectile_body: Body,
        projectile_light: Option<LightEmitter>,
        initial_projectile_speed: f32,
        scaled_projectile_speed: f32,
        projectile_spread: Option<ProjectileSpread>,
        #[serde(default)]
        num_projectiles: Amount,
        marker: Option<comp::FrontendMarker>,
        move_speed: f32,
        #[serde(default)]
        meta: AbilityMeta,
    },
    Throw {
        energy_cost: f32,
        energy_drain: f32,
        buildup_duration: f32,
        charge_duration: f32,
        throw_duration: f32,
        recover_duration: f32,
        projectile: ProjectileConstructor,
        projectile_light: Option<LightEmitter>,
        projectile_dir: throw::ProjectileDir,
        initial_projectile_speed: f32,
        scaled_projectile_speed: f32,
        damage_effect: Option<CombatEffect>,
        move_speed: f32,
        #[serde(default)]
        meta: AbilityMeta,
    },
    TelekineticGrip {
        energy_cost: f32,
        energy_drain: f32,
        buildup_duration: f32,
        charge_duration: f32,
        place_threshold: f32,
        recover_duration: f32,
        range: f32,
        tether_length: f32,
        initial_projectile_speed: f32,
        scaled_projectile_speed: f32,
        projectile: ProjectileConstructor,
        move_speed: f32,
        #[serde(default)]
        meta: AbilityMeta,
    },
    Shockwave {
        energy_cost: f32,
        buildup_duration: f32,
        swing_duration: f32,
        recover_duration: f32,
        damage: f32,
        poise_damage: f32,
        knockback: Knockback,
        shockwave_angle: f32,
        shockwave_vertical_angle: f32,
        shockwave_speed: f32,
        shockwave_duration: f32,
        dodgeable: Dodgeable,
        move_efficiency: f32,
        damage_kind: DamageKind,
        specifier: comp::shockwave::FrontendSpecifier,
        ori_rate: f32,
        damage_effect: Option<CombatEffect>,
        timing: shockwave::Timing,
        emit_outcome: bool,
        minimum_combo: Option<u32>,
        #[serde(default)]
        combo_consumption: ComboConsumption,
        #[serde(default)]
        meta: AbilityMeta,
    },
    Explosion {
        energy_cost: f32,
        buildup_duration: f32,
        action_duration: f32,
        recover_duration: f32,
        damage: f32,
        poise: f32,
        knockback: Knockback,
        radius: f32,
        min_falloff: f32,
        #[serde(default)]
        dodgeable: Dodgeable,
        #[serde(default)]
        destroy_terrain: Option<(f32, ColorPreset)>,
        #[serde(default)]
        replace_terrain: Option<(f32, TerrainReplacementPreset)>,
        #[serde(default)]
        eye_height: bool,
        #[serde(default)]
        reagent: Option<Reagent>,
        #[serde(default)]
        movement_modifier: MovementModifier,
        #[serde(default)]
        ori_modifier: OrientationModifier,
        #[serde(default)]
        meta: AbilityMeta,
    },
    GroundAoe {
        energy_cost: f32,
        buildup_duration: f32,
        /// Telegraph time between target lock and the strike
        delay: f32,
        recover_duration: f32,
        max_range: f32,
        radius: f32,
        min_falloff: f32,
        damage: f32,
        poise: f32,
        knockback: Knockback,
        #[serde(default)]
        dodgeable: Dodgeable,
        #[serde(default)]
        reagent: Option<Reagent>,
        /// If true the caster cannot move during the telegraph
        #[serde(default)]
        rooted_cast: bool,
        #[serde(default)]
        meta: AbilityMeta,
    },
    BasicBeam {
        buildup_duration: f32,
        recover_duration: f32,
        beam_duration: f64,
        damage: f32,
        tick_rate: f32,
        range: f32,
        #[serde(default)]
        dodgeable: Dodgeable,
        #[serde(default = "default_true")]
        blockable: bool,
        max_angle: f32,
        damage_effect: Option<CombatEffect>,
        energy_regen: f32,
        energy_drain: f32,
        ori_rate: f32,
        move_efficiency: f32,
        specifier: beam::FrontendSpecifier,
        #[serde(default)]
        meta: AbilityMeta,
    },
    BasicAura {
        buildup_duration: f32,
        cast_duration: f32,
        recover_duration: f32,
        targets: combat::GroupTarget,
        auras: Vec<aura::AuraBuffConstructor>,
        /// Capped-nearest-N, per-target-tiered effects (see
        /// `aura::AuraKind::TieredHealthEffect`) created alongside `auras`.
        /// Kept as a separate list since these build a different `AuraKind`
        /// variant entirely, not a `Buff`.
        #[serde(default)]
        tiered_health_effects: Vec<aura::TieredHealthEffectConstructor>,
        aura_duration: Option<Secs>,
        range: f32,
        energy_cost: f32,
        scales_with_combo: bool,
        specifier: Option<aura::Specifier>,
        #[serde(default)]
        meta: AbilityMeta,
    },
    StaticAura {
        buildup_duration: f32,
        cast_duration: f32,
        recover_duration: f32,
        energy_cost: f32,
        targets: combat::GroupTarget,
        auras: Vec<aura::AuraBuffConstructor>,
        aura_duration: Option<Secs>,
        range: f32,
        sprite_info: Option<static_aura::SpriteInfo>,
        #[serde(default)]
        meta: AbilityMeta,
    },
    Blink {
        buildup_duration: f32,
        recover_duration: f32,
        max_range: f32,
        frontend_specifier: Option<blink::FrontendSpecifier>,
        #[serde(default)]
        meta: AbilityMeta,
    },
    BasicSummon {
        buildup_duration: f32,
        cast_duration: f32,
        recover_duration: f32,
        summon_info: basic_summon::SummonInfo,
        #[serde(default)]
        movement_modifier: MovementModifier,
        #[serde(default)]
        ori_modifier: OrientationModifier,
        #[serde(default)]
        meta: AbilityMeta,
    },
    SelfBuff {
        buildup_duration: f32,
        cast_duration: f32,
        recover_duration: f32,
        buffs: Vec<self_buff::BuffDesc>,
        #[serde(default)]
        use_raw_buff_strength: bool,
        buff_cat: Option<buff::BuffCategory>,
        energy_cost: f32,
        #[serde(default = "default_true")]
        enforced_limit: bool,
        #[serde(default)]
        combo_cost: u32,
        combo_scaling: Option<ScalingKind>,
        #[serde(default)]
        meta: AbilityMeta,
        specifier: Option<self_buff::FrontendSpecifier>,
    },
    SpriteSummon {
        buildup_duration: f32,
        cast_duration: f32,
        recover_duration: f32,
        sprite: SpriteKind,
        del_timeout: Option<(f32, f32)>,
        summon_distance: (f32, f32),
        sparseness: f64,
        angle: f32,
        #[serde(default)]
        anchor: SpriteSummonAnchor,
        #[serde(default)]
        move_efficiency: f32,
        ori_modifier: f32,
        #[serde(default)]
        meta: AbilityMeta,
    },
    /// A single-shot, ranged/keyless unlock effect targeted at a sprite
    /// position (e.g. the `knock` spell). See `common/src/states/knock.rs`.
    Knock {
        energy_cost: f32,
        buildup_duration: f32,
        cast_duration: f32,
        recover_duration: f32,
        /// Max range the targeted sprite position can be from the caster
        range: f32,
        #[serde(default)]
        ori_modifier: f32,
        #[serde(default)]
        meta: AbilityMeta,
    },
    Music {
        play_duration: f32,
        ori_modifier: f32,
        #[serde(default)]
        meta: AbilityMeta,
    },
    FinisherMelee {
        energy_cost: f32,
        buildup_duration: f32,
        swing_duration: f32,
        recover_duration: f32,
        melee_constructor: MeleeConstructor,
        minimum_combo: u32,
        scaling: Option<finisher_melee::Scaling>,
        #[serde(default)]
        combo_consumption: ComboConsumption,
        #[serde(default)]
        meta: AbilityMeta,
    },
    DiveMelee {
        energy_cost: f32,
        vertical_speed: f32,
        buildup_duration: Option<f32>,
        movement_duration: f32,
        swing_duration: f32,
        recover_duration: f32,
        melee_constructor: MeleeConstructor,
        max_scaling: f32,
        #[serde(default)]
        meta: AbilityMeta,
    },
    RiposteMelee {
        energy_cost: f32,
        buildup_duration: f32,
        swing_duration: f32,
        recover_duration: f32,
        whiffed_recover_duration: f32,
        block_strength: f32,
        melee_constructor: MeleeConstructor,
        #[serde(default)]
        meta: AbilityMeta,
    },
    RapidMelee {
        buildup_duration: f32,
        swing_duration: f32,
        recover_duration: f32,
        energy_cost: f32,
        max_strikes: Option<u32>,
        melee_constructor: MeleeConstructor,
        move_modifier: f32,
        ori_modifier: f32,
        frontend_specifier: Option<rapid_melee::FrontendSpecifier>,
        #[serde(default)]
        minimum_combo: u32,
        #[serde(default)]
        meta: AbilityMeta,
    },
    Transform {
        buildup_duration: f32,
        recover_duration: f32,
        target: String,
        #[serde(default)]
        specifier: Option<transform::FrontendSpecifier>,
        /// Only set to `true` for admin only abilities since this disables
        /// persistence and is not intended to be used by regular players
        #[serde(default)]
        allow_players: bool,
        #[serde(default)]
        meta: AbilityMeta,
    },
    RegrowHead {
        buildup_duration: f32,
        recover_duration: f32,
        energy_cost: f32,
        #[serde(default)]
        specifier: Option<regrow_head::FrontendSpecifier>,
        #[serde(default)]
        meta: AbilityMeta,
    },
    LeapRanged {
        energy_cost: f32,
        buildup_duration: f32,
        buildup_melee_timing: f32,
        movement_duration: f32,
        movement_ranged_timing: f32,
        land_timeout: f32,
        recover_duration: f32,
        melee: Option<MeleeConstructor>,
        melee_required: bool,
        projectile: ProjectileConstructor,
        projectile_body: Body,
        projectile_light: Option<LightEmitter>,
        projectile_speed: f32,
        horiz_leap_strength: f32,
        vert_leap_strength: f32,
        #[serde(default)]
        meta: AbilityMeta,
    },
    Simple {
        energy_cost: f32,
        combo_cost: u32,
        buildup_duration: f32,
        #[serde(default)]
        meta: AbilityMeta,
    },
}

impl Default for CharacterAbility {
    fn default() -> Self {
        CharacterAbility::BasicMelee {
            energy_cost: 0.0,
            buildup_duration: 0.25,
            swing_duration: 0.25,
            hit_timing: 0.5,
            recover_duration: 0.5,
            melee_constructor: MeleeConstructor {
                kind: MeleeConstructorKind::Slash {
                    damage: 1.0,
                    knockback: 0.0,
                    poise: 0.0,
                    energy_regen: 0.0,
                },
                scaled: None,
                range: 3.5,
                angle: 15.0,
                multi_target: None,
                damage_effect: None,
                attack_effect: None,
                simultaneous_hits: 1,
                custom_combo: CustomCombo {
                    base: None,
                    conditional: None,
                },
                dodgeable: Dodgeable::Roll,
                blockable: true,
                precision_flank_multipliers: Default::default(),
                precision_flank_invert: false,
            },
            movement_modifier: Default::default(),
            ori_modifier: Default::default(),
            frontend_specifier: None,
            meta: Default::default(),
        }
    }
}

impl CharacterAbility {
    /// Attempts to fulfill requirements, mutating `update` (taking energy) if
    /// applicable.
    pub fn requirements_paid(&self, data: &JoinData, update: &mut StateUpdate) -> bool {
        let from_meta = {
            let AbilityMeta { requirements, .. } = self.ability_meta();
            requirements.requirements_met(
                data.stance,
                data.inventory,
                data.oracle_live.0,
                data.skill_set.character_level(),
            )
        };
        from_meta
            && match self {
                CharacterAbility::Roll { energy_cost, .. }
                | CharacterAbility::StaticAura {
                    energy_cost,
                    sprite_info: Some(_),
                    ..
                } => {
                    data.physics.on_ground.is_some()
                        && update.energy.try_change_by(-*energy_cost).is_ok()
                },
                CharacterAbility::DashMelee { energy_cost, .. }
                | CharacterAbility::BasicMelee { energy_cost, .. }
                | CharacterAbility::BasicRanged { energy_cost, .. }
                | CharacterAbility::ChargedRanged { energy_cost, .. }
                | CharacterAbility::Throw { energy_cost, .. }
                | CharacterAbility::TelekineticGrip { energy_cost, .. }
                | CharacterAbility::ChargedMelee { energy_cost, .. }
                | CharacterAbility::BasicBlock { energy_cost, .. }
                | CharacterAbility::RiposteMelee { energy_cost, .. }
                | CharacterAbility::ComboMelee2 {
                    energy_cost_per_strike: energy_cost,
                    ..
                }
                | CharacterAbility::StaticAura {
                    energy_cost,
                    sprite_info: None,
                    ..
                }
                | CharacterAbility::RegrowHead { energy_cost, .. } => {
                    update.energy.try_change_by(-*energy_cost).is_ok()
                },
                // Also can consume energy within state, so value checked before entering state too
                CharacterAbility::RapidRanged {
                    initial_energy,
                    energy_cost,
                    ..
                } => {
                    update.energy.current() >= *energy_cost + *initial_energy
                        && update.energy.try_change_by(-*initial_energy).is_ok()
                },
                CharacterAbility::LeapExplosionShockwave { energy_cost, .. }
                | CharacterAbility::LeapMelee { energy_cost, .. }
                | CharacterAbility::LeapShockwave { energy_cost, .. }
                | CharacterAbility::LeapRanged { energy_cost, .. } => {
                    update.vel.0.z >= 0.0 && update.energy.try_change_by(-*energy_cost).is_ok()
                },
                CharacterAbility::BasicAura {
                    energy_cost,
                    scales_with_combo,
                    ..
                } => {
                    ((*scales_with_combo && data.combo.is_some_and(|c| c.counter() > 0))
                        | !*scales_with_combo)
                        && update.energy.try_change_by(-*energy_cost).is_ok()
                },
                CharacterAbility::FinisherMelee {
                    energy_cost,
                    minimum_combo,
                    ..
                }
                | CharacterAbility::RapidMelee {
                    energy_cost,
                    minimum_combo,
                    ..
                }
                | CharacterAbility::SelfBuff {
                    energy_cost,
                    combo_cost: minimum_combo,
                    ..
                }
                | CharacterAbility::Simple {
                    energy_cost,
                    combo_cost: minimum_combo,
                    ..
                } => {
                    data.combo.is_some_and(|c| c.counter() >= *minimum_combo)
                        && update.energy.try_change_by(-*energy_cost).is_ok()
                },
                CharacterAbility::Shockwave {
                    energy_cost,
                    minimum_combo,
                    ..
                } => {
                    data.combo
                        .is_some_and(|c| c.counter() >= minimum_combo.unwrap_or(0))
                        && update.energy.try_change_by(-*energy_cost).is_ok()
                },
                CharacterAbility::Explosion { energy_cost, .. } => {
                    update.energy.try_change_by(-*energy_cost).is_ok()
                },
                CharacterAbility::GroundAoe { energy_cost, .. } => {
                    update.energy.try_change_by(-*energy_cost).is_ok()
                },
                CharacterAbility::Knock { energy_cost, .. } => {
                    update.energy.try_change_by(-*energy_cost).is_ok()
                },
                CharacterAbility::DiveMelee {
                    buildup_duration,
                    energy_cost,
                    ..
                } => {
                    // If either in the air or is on ground and able to be activated from
                    // ground.
                    //
                    // NOTE: there is a check in CharacterState::try_from below that must be kept in
                    // sync with the conditions here (it determines whether this starts in a
                    // movement or buildup stage).
                    (data.physics.on_ground.is_none() || buildup_duration.is_some())
                        && update.energy.try_change_by(-*energy_cost).is_ok()
                },
                CharacterAbility::Boost { .. }
                | CharacterAbility::GlideBoost { .. }
                | CharacterAbility::BasicBeam { .. }
                | CharacterAbility::Blink { .. }
                | CharacterAbility::Music { .. }
                | CharacterAbility::BasicSummon { .. }
                | CharacterAbility::SpriteSummon { .. }
                | CharacterAbility::Transform { .. } => true,
            }
    }

    pub fn default_roll(current_state: Option<&CharacterState>) -> CharacterAbility {
        let remaining_duration = current_state
            .and_then(|char_state| {
                char_state.timer().zip(
                    char_state
                        .durations()
                        .zip(char_state.stage_section())
                        .and_then(|(durations, stage_section)| match stage_section {
                            StageSection::Buildup => durations.buildup,
                            StageSection::Recover => durations.recover,
                            _ => None,
                        }),
                )
            })
            .map_or(0.0, |(timer, duration)| {
                duration.as_secs_f32() - timer.as_secs_f32()
            })
            .max(0.0);

        CharacterAbility::Roll {
            // Energy cost increased by remaining duration
            energy_cost: 10.0 + 100.0 * remaining_duration,
            buildup_duration: 0.05,
            movement_duration: 0.36,
            recover_duration: 0.125,
            roll_strength: 3.3075,
            attack_immunities: AttackFilters {
                melee: true,
                projectiles: false,
                beams: true,
                ground_shockwaves: false,
                air_shockwaves: true,
                explosions: true,
                arcs: true,
                pools: true,
            },
            was_cancel: remaining_duration > 0.0,
            meta: Default::default(),
        }
    }

    #[must_use]
    pub fn adjusted_by_stats(mut self, stats: Stats) -> Self {
        use CharacterAbility::*;
        match self {
            BasicMelee {
                ref mut energy_cost,
                ref mut buildup_duration,
                ref mut swing_duration,
                ref mut recover_duration,
                ref mut melee_constructor,
                movement_modifier: _,
                ori_modifier: _,
                hit_timing: _,
                frontend_specifier: _,
                meta: _,
            } => {
                *buildup_duration /= stats.speed;
                *swing_duration /= stats.speed;
                *recover_duration /= stats.speed;
                *energy_cost /= stats.energy_efficiency;
                *melee_constructor = melee_constructor.clone().adjusted_by_stats(stats);
            },
            BasicRanged {
                ref mut energy_cost,
                ref mut buildup_duration,
                ref mut recover_duration,
                ref mut projectile,
                projectile_body: _,
                projectile_light: _,
                ref mut projectile_speed,
                vertical_angle_offset: _,
                num_projectiles: _,
                projectile_spread: _,
                auto_aim: _,
                movement_modifier: _,
                ori_modifier: _,
                marker: _,
                meta: _,
            } => {
                *buildup_duration /= stats.speed;
                *recover_duration /= stats.speed;
                *projectile = projectile.clone().adjusted_by_stats(stats);
                *projectile_speed *= stats.range;
                *energy_cost /= stats.energy_efficiency;
            },
            RapidRanged {
                ref mut initial_energy,
                ref mut energy_cost,
                ref mut buildup_duration,
                ref mut shoot_duration,
                ref mut recover_duration,
                options: _,
                ref mut projectile,
                projectile_body: _,
                projectile_light: _,
                ref mut projectile_speed,
                specifier: _,
                meta: _,
            } => {
                *buildup_duration /= stats.speed;
                *shoot_duration /= stats.speed;
                *recover_duration /= stats.speed;
                *projectile = projectile.clone().adjusted_by_stats(stats);
                *projectile_speed *= stats.range;
                *initial_energy /= stats.energy_efficiency;
                *energy_cost /= stats.energy_efficiency;
            },
            Boost {
                ref mut movement_duration,
                only_up: _,
                speed: ref mut boost_speed,
                max_exit_velocity: _,
                meta: _,
            } => {
                *movement_duration /= stats.speed;
                *boost_speed *= stats.power;
            },
            DashMelee {
                ref mut energy_cost,
                ref mut energy_drain,
                forward_speed: _,
                ref mut buildup_duration,
                charge_duration: _,
                ref mut swing_duration,
                ref mut recover_duration,
                ref mut melee_constructor,
                ori_modifier: _,
                auto_charge: _,
                charge_through: _,
                frontend_specifier: _,
                meta: _,
            } => {
                *buildup_duration /= stats.speed;
                *swing_duration /= stats.speed;
                *recover_duration /= stats.speed;
                *energy_cost /= stats.energy_efficiency;
                *energy_drain /= stats.energy_efficiency;
                *melee_constructor = melee_constructor.clone().adjusted_by_stats(stats);
            },
            BasicBlock {
                ref mut buildup_duration,
                ref mut recover_duration,
                // Do we want angle to be adjusted by range?
                max_angle: _,
                ref mut block_strength,
                parry_window: _,
                ref mut energy_cost,
                energy_regen: _,
                can_hold: _,
                blocked_attacks: _,
                meta: _,
            } => {
                *buildup_duration /= stats.speed;
                *recover_duration /= stats.speed;
                *energy_cost /= stats.energy_efficiency;
                *block_strength *= stats.power;
            },
            Roll {
                ref mut energy_cost,
                ref mut buildup_duration,
                ref mut movement_duration,
                ref mut recover_duration,
                roll_strength: _,
                attack_immunities: _,
                was_cancel: _,
                meta: _,
            } => {
                *buildup_duration /= stats.speed;
                *movement_duration /= stats.speed;
                *recover_duration /= stats.speed;
                *energy_cost /= stats.energy_efficiency;
            },
            ComboMelee2 {
                ref mut strikes,
                ref mut energy_cost_per_strike,
                specifier: _,
                auto_progress: _,
                meta: _,
            } => {
                *energy_cost_per_strike /= stats.energy_efficiency;
                *strikes = strikes
                    .iter_mut()
                    .map(|s| s.clone().adjusted_by_stats(stats))
                    .collect();
            },
            LeapExplosionShockwave {
                ref mut energy_cost,
                ref mut buildup_duration,
                ref mut movement_duration,
                ref mut swing_duration,
                ref mut recover_duration,
                forward_leap_strength: _,
                vertical_leap_strength: _,
                ref mut explosion_damage,
                ref mut explosion_poise,
                ref mut explosion_knockback,
                ref mut explosion_radius,
                min_falloff: _,
                explosion_dodgeable: _,
                destroy_terrain: _,
                replace_terrain: _,
                eye_height: _,
                reagent: _,
                ref mut shockwave_damage,
                ref mut shockwave_poise,
                ref mut shockwave_knockback,
                shockwave_angle: _,
                shockwave_vertical_angle: _,
                shockwave_speed: _,
                ref mut shockwave_duration,
                shockwave_dodgeable: _,
                ref mut shockwave_damage_effect,
                shockwave_damage_kind: _,
                shockwave_specifier: _,
                move_efficiency: _,
                meta: _,
            } => {
                *energy_cost /= stats.energy_efficiency;
                *buildup_duration /= stats.speed;
                *movement_duration /= stats.speed;
                *swing_duration /= stats.speed;
                *recover_duration /= stats.speed;

                *explosion_damage *= stats.power;
                *explosion_poise *= stats.effect_power;
                explosion_knockback.strength *= stats.effect_power;
                *explosion_radius *= stats.range;

                *shockwave_damage *= stats.power;
                *shockwave_poise *= stats.effect_power;
                shockwave_knockback.strength *= stats.effect_power;
                *shockwave_duration *= stats.range;
                if let Some(CombatEffect::Buff(combat::CombatBuff {
                    kind: _,
                    dur_secs: _,
                    strength,
                    chance: _,
                })) = shockwave_damage_effect
                {
                    *strength *= stats.buff_strength;
                }
            },
            LeapMelee {
                ref mut energy_cost,
                ref mut buildup_duration,
                movement_duration: _,
                ref mut swing_duration,
                ref mut recover_duration,
                ref mut melee_constructor,
                forward_leap_strength: _,
                vertical_leap_strength: _,
                specifier: _,
                meta: _,
            } => {
                *buildup_duration /= stats.speed;
                *swing_duration /= stats.speed;
                *recover_duration /= stats.speed;
                *energy_cost /= stats.energy_efficiency;
                *melee_constructor = melee_constructor.clone().adjusted_by_stats(stats)
            },
            LeapShockwave {
                ref mut energy_cost,
                ref mut buildup_duration,
                movement_duration: _,
                ref mut swing_duration,
                ref mut recover_duration,
                ref mut damage,
                ref mut poise_damage,
                knockback: _,
                shockwave_angle: _,
                shockwave_vertical_angle: _,
                shockwave_speed: _,
                ref mut shockwave_duration,
                dodgeable: _,
                move_efficiency: _,
                damage_kind: _,
                specifier: _,
                ref mut damage_effect,
                forward_leap_strength: _,
                vertical_leap_strength: _,
                meta: _,
            } => {
                *buildup_duration /= stats.speed;
                *swing_duration /= stats.speed;
                *recover_duration /= stats.speed;
                *damage *= stats.power;
                *poise_damage *= stats.effect_power;
                *shockwave_duration *= stats.range;
                *energy_cost /= stats.energy_efficiency;
                if let Some(CombatEffect::Buff(combat::CombatBuff {
                    kind: _,
                    dur_secs: _,
                    strength,
                    chance: _,
                })) = damage_effect
                {
                    *strength *= stats.buff_strength;
                }
            },
            ChargedMelee {
                ref mut energy_cost,
                ref mut energy_drain,
                ref mut buildup_strike,
                ref mut charge_duration,
                ref mut swing_duration,
                hit_timing: _,
                ref mut recover_duration,
                ref mut melee_constructor,
                specifier: _,
                meta: _,
                custom_combo: _,
                movement_modifier: _,
                ori_modifier: _,
            } => {
                *swing_duration /= stats.speed;
                *buildup_strike = buildup_strike
                    .as_ref()
                    .cloned()
                    .map(|(dur, strike)| (dur / stats.speed, strike.adjusted_by_stats(stats)));
                *charge_duration /= stats.speed;
                *recover_duration /= stats.speed;
                *energy_cost /= stats.energy_efficiency;
                *energy_drain *= stats.speed / stats.energy_efficiency;
                *melee_constructor = melee_constructor.clone().adjusted_by_stats(stats);
            },
            ChargedRanged {
                ref mut energy_cost,
                ref mut energy_drain,
                ref mut idle_drain,
                ref mut projectile,
                ref mut buildup_duration,
                ref mut charge_duration,
                ref mut recover_duration,
                projectile_body: _,
                projectile_light: _,
                ref mut initial_projectile_speed,
                ref mut scaled_projectile_speed,
                projectile_spread: _,
                num_projectiles: _,
                marker: _,
                move_speed: _,
                meta: _,
            } => {
                *projectile = projectile.clone().adjusted_by_stats(stats);
                *buildup_duration /= stats.speed;
                *charge_duration /= stats.speed;
                *recover_duration /= stats.speed;
                *initial_projectile_speed *= stats.range;
                *scaled_projectile_speed *= stats.range;
                *energy_cost /= stats.energy_efficiency;
                *energy_drain *= stats.speed / stats.energy_efficiency;
                *idle_drain /= stats.energy_efficiency;
            },
            Throw {
                ref mut energy_cost,
                ref mut energy_drain,
                ref mut buildup_duration,
                ref mut charge_duration,
                ref mut throw_duration,
                ref mut recover_duration,
                ref mut projectile,
                projectile_light: _,
                projectile_dir: _,
                ref mut initial_projectile_speed,
                ref mut scaled_projectile_speed,
                damage_effect: _,
                move_speed: _,
                meta: _,
            } => {
                *projectile = projectile.clone().adjusted_by_stats(stats);
                *energy_cost /= stats.energy_efficiency;
                *energy_drain *= stats.speed / stats.energy_efficiency;
                *buildup_duration /= stats.speed;
                *charge_duration /= stats.speed;
                *throw_duration /= stats.speed;
                *recover_duration /= stats.speed;
                *initial_projectile_speed *= stats.range;
                *scaled_projectile_speed *= stats.range;
            },
            TelekineticGrip {
                ref mut energy_cost,
                ref mut energy_drain,
                ref mut buildup_duration,
                ref mut charge_duration,
                ref mut place_threshold,
                ref mut recover_duration,
                ref mut range,
                ref mut tether_length,
                ref mut initial_projectile_speed,
                ref mut scaled_projectile_speed,
                ref mut projectile,
                move_speed: _,
                meta: _,
            } => {
                *projectile = projectile.clone().adjusted_by_stats(stats);
                *energy_cost /= stats.energy_efficiency;
                *energy_drain *= stats.speed / stats.energy_efficiency;
                *buildup_duration /= stats.speed;
                *charge_duration /= stats.speed;
                *place_threshold /= stats.speed;
                *recover_duration /= stats.speed;
                *range *= stats.range;
                *tether_length *= stats.range;
                *initial_projectile_speed *= stats.range;
                *scaled_projectile_speed *= stats.range;
            },
            Shockwave {
                ref mut energy_cost,
                ref mut buildup_duration,
                ref mut swing_duration,
                ref mut recover_duration,
                ref mut damage,
                ref mut poise_damage,
                knockback: _,
                shockwave_angle: _,
                shockwave_vertical_angle: _,
                shockwave_speed: _,
                ref mut shockwave_duration,
                dodgeable: _,
                move_efficiency: _,
                damage_kind: _,
                specifier: _,
                ori_rate: _,
                ref mut damage_effect,
                timing: _,
                emit_outcome: _,
                minimum_combo: _,
                combo_consumption: _,
                meta: _,
            } => {
                *buildup_duration /= stats.speed;
                *swing_duration /= stats.speed;
                *recover_duration /= stats.speed;
                *damage *= stats.power;
                *poise_damage *= stats.effect_power;
                *shockwave_duration *= stats.range;
                *energy_cost /= stats.energy_efficiency;
                *damage_effect = damage_effect
                    .as_ref()
                    .cloned()
                    .map(|de| de.adjusted_by_stats(stats));
            },
            Explosion {
                ref mut energy_cost,
                ref mut buildup_duration,
                ref mut action_duration,
                ref mut recover_duration,
                ref mut damage,
                poise: ref mut poise_damage,
                ref mut knockback,
                ref mut radius,
                min_falloff: _,
                dodgeable: _,
                destroy_terrain: _,
                replace_terrain: _,
                eye_height: _,
                reagent: _,
                movement_modifier: _,
                ori_modifier: _,
                meta: _,
            } => {
                *energy_cost /= stats.energy_efficiency;
                *buildup_duration /= stats.speed;
                *action_duration /= stats.speed;
                *recover_duration /= stats.speed;
                *damage *= stats.power;
                *poise_damage *= stats.effect_power;
                knockback.strength *= stats.effect_power;
                *radius *= stats.range;
            },
            GroundAoe {
                ref mut energy_cost,
                ref mut buildup_duration,
                delay: _,
                ref mut recover_duration,
                ref mut max_range,
                ref mut radius,
                min_falloff: _,
                ref mut damage,
                poise: ref mut poise_damage,
                ref mut knockback,
                dodgeable: _,
                reagent: _,
                rooted_cast: _,
                meta: _,
            } => {
                *energy_cost /= stats.energy_efficiency;
                *buildup_duration /= stats.speed;
                *recover_duration /= stats.speed;
                *damage *= stats.power;
                *poise_damage *= stats.effect_power;
                knockback.strength *= stats.effect_power;
                *radius *= stats.range;
                *max_range *= stats.range;
            },
            BasicBeam {
                ref mut buildup_duration,
                ref mut recover_duration,
                ref mut beam_duration,
                ref mut damage,
                ref mut tick_rate,
                ref mut range,
                dodgeable: _,
                blockable: _,
                max_angle: _,
                ref mut damage_effect,
                energy_regen: _,
                ref mut energy_drain,
                move_efficiency: _,
                ori_rate: _,
                specifier: _,
                meta: _,
            } => {
                *buildup_duration /= stats.speed;
                *recover_duration /= stats.speed;
                *damage *= stats.power;
                *tick_rate *= stats.speed;
                *range *= stats.range;
                // Duration modified to keep velocity constant
                *beam_duration *= stats.range as f64;
                *energy_drain /= stats.energy_efficiency;
                *damage_effect = damage_effect
                    .as_ref()
                    .cloned()
                    .map(|de| de.adjusted_by_stats(stats));
            },
            BasicAura {
                ref mut buildup_duration,
                ref mut cast_duration,
                ref mut recover_duration,
                targets: _,
                ref mut auras,
                tiered_health_effects: _,
                aura_duration: _,
                ref mut range,
                ref mut energy_cost,
                scales_with_combo: _,
                specifier: _,
                meta: _,
            } => {
                *buildup_duration /= stats.speed;
                *cast_duration /= stats.speed;
                *recover_duration /= stats.speed;
                auras.iter_mut().for_each(
                    |aura::AuraBuffConstructor {
                         kind: _,
                         strength,
                         duration: _,
                         category: _,
                         pool_split,
                     }| {
                        *strength *= stats.diminished_buff_strength();
                        if let Some(split) = pool_split {
                            split.value_at_unlock *= stats.diminished_buff_strength();
                            split.value_at_max_level *= stats.diminished_buff_strength();
                        }
                    },
                );
                *range *= stats.range;
                *energy_cost /= stats.energy_efficiency;
            },
            StaticAura {
                ref mut buildup_duration,
                ref mut cast_duration,
                ref mut recover_duration,
                targets: _,
                ref mut auras,
                aura_duration: _,
                ref mut range,
                ref mut energy_cost,
                ref mut sprite_info,
                meta: _,
            } => {
                *buildup_duration /= stats.speed;
                *cast_duration /= stats.speed;
                *recover_duration /= stats.speed;
                auras.iter_mut().for_each(
                    |aura::AuraBuffConstructor {
                         kind: _,
                         strength,
                         duration: _,
                         category: _,
                         pool_split,
                     }| {
                        *strength *= stats.diminished_buff_strength();
                        if let Some(split) = pool_split {
                            split.value_at_unlock *= stats.diminished_buff_strength();
                            split.value_at_max_level *= stats.diminished_buff_strength();
                        }
                    },
                );
                *range *= stats.range;
                *energy_cost /= stats.energy_efficiency;
                *sprite_info = sprite_info.map(|mut si| {
                    si.summon_distance.0 *= stats.range;
                    si.summon_distance.1 *= stats.range;
                    si
                });
            },
            Blink {
                ref mut buildup_duration,
                ref mut recover_duration,
                ref mut max_range,
                frontend_specifier: _,
                meta: _,
            } => {
                *buildup_duration /= stats.speed;
                *recover_duration /= stats.speed;
                *max_range *= stats.range;
            },
            BasicSummon {
                ref mut buildup_duration,
                ref mut cast_duration,
                ref mut recover_duration,
                ref mut summon_info,
                movement_modifier: _,
                ori_modifier: _,
                meta: _,
            } => {
                // TODO: Figure out how/if power should affect this
                *buildup_duration /= stats.speed;
                *cast_duration /= stats.speed;
                *recover_duration /= stats.speed;
                summon_info.scale_range(stats.range);
            },
            SelfBuff {
                ref mut buildup_duration,
                ref mut cast_duration,
                ref mut recover_duration,
                ref mut buffs,
                use_raw_buff_strength,
                buff_cat: _,
                ref mut energy_cost,
                enforced_limit: _,
                combo_cost: _,
                combo_scaling: _,
                meta: _,
                specifier: _,
            } => {
                for buff in buffs.iter_mut() {
                    buff.data.strength *= if use_raw_buff_strength {
                        stats.buff_strength
                    } else {
                        stats.diminished_buff_strength()
                    };
                }
                *buildup_duration /= stats.speed;
                *cast_duration /= stats.speed;
                *recover_duration /= stats.speed;
                *energy_cost /= stats.energy_efficiency;
            },
            SpriteSummon {
                ref mut buildup_duration,
                ref mut cast_duration,
                ref mut recover_duration,
                sprite: _,
                del_timeout: _,
                summon_distance: (ref mut inner_dist, ref mut outer_dist),
                sparseness: _,
                angle: _,
                anchor: _,
                move_efficiency: _,
                ori_modifier: _,
                meta: _,
            } => {
                // TODO: Figure out how/if power should affect this
                *buildup_duration /= stats.speed;
                *cast_duration /= stats.speed;
                *recover_duration /= stats.speed;
                *inner_dist *= stats.range;
                *outer_dist *= stats.range;
            },
            Music {
                ref mut play_duration,
                ori_modifier: _,
                meta: _,
            } => {
                *play_duration /= stats.speed;
            },
            FinisherMelee {
                ref mut energy_cost,
                ref mut buildup_duration,
                ref mut swing_duration,
                ref mut recover_duration,
                ref mut melee_constructor,
                minimum_combo: _,
                scaling: _,
                combo_consumption: _,
                meta: _,
            } => {
                *buildup_duration /= stats.speed;
                *swing_duration /= stats.speed;
                *recover_duration /= stats.speed;
                *energy_cost /= stats.energy_efficiency;
                *melee_constructor = melee_constructor.clone().adjusted_by_stats(stats);
            },
            DiveMelee {
                ref mut energy_cost,
                vertical_speed: _,
                movement_duration: _,
                ref mut buildup_duration,
                ref mut swing_duration,
                ref mut recover_duration,
                ref mut melee_constructor,
                max_scaling: _,
                meta: _,
            } => {
                *buildup_duration = buildup_duration.map(|b| b / stats.speed);
                *swing_duration /= stats.speed;
                *recover_duration /= stats.speed;
                *energy_cost /= stats.energy_efficiency;
                *melee_constructor = melee_constructor.clone().adjusted_by_stats(stats);
            },
            RiposteMelee {
                ref mut energy_cost,
                ref mut buildup_duration,
                ref mut swing_duration,
                ref mut recover_duration,
                ref mut whiffed_recover_duration,
                ref mut block_strength,
                ref mut melee_constructor,
                meta: _,
            } => {
                *buildup_duration /= stats.speed;
                *swing_duration /= stats.speed;
                *recover_duration /= stats.speed;
                *whiffed_recover_duration /= stats.speed;
                *energy_cost /= stats.energy_efficiency;
                *block_strength *= stats.power;
                *melee_constructor = melee_constructor.clone().adjusted_by_stats(stats);
            },
            RapidMelee {
                ref mut buildup_duration,
                ref mut swing_duration,
                ref mut recover_duration,
                ref mut energy_cost,
                ref mut melee_constructor,
                max_strikes: _,
                move_modifier: _,
                ori_modifier: _,
                minimum_combo: _,
                frontend_specifier: _,
                meta: _,
            } => {
                *buildup_duration /= stats.speed;
                *swing_duration /= stats.speed;
                *recover_duration /= stats.speed;
                *energy_cost /= stats.energy_efficiency;
                *melee_constructor = melee_constructor.clone().adjusted_by_stats(stats);
            },
            Transform {
                ref mut buildup_duration,
                ref mut recover_duration,
                target: _,
                specifier: _,
                allow_players: _,
                meta: _,
            } => {
                *buildup_duration /= stats.speed;
                *recover_duration /= stats.speed;
            },
            GlideBoost { .. } => {},
            RegrowHead {
                ref mut buildup_duration,
                ref mut recover_duration,
                ref mut energy_cost,
                specifier: _,
                meta: _,
            } => {
                *buildup_duration /= stats.speed;
                *recover_duration /= stats.speed;
                *energy_cost /= stats.energy_efficiency;
            },
            LeapRanged {
                ref mut energy_cost,
                ref mut buildup_duration,
                buildup_melee_timing: _,
                movement_duration: _,
                movement_ranged_timing: _,
                land_timeout: _,
                ref mut recover_duration,
                ref mut melee,
                melee_required: _,
                ref mut projectile,
                projectile_body: _,
                projectile_light: _,
                ref mut projectile_speed,
                horiz_leap_strength: _,
                vert_leap_strength: _,
                meta: _,
            } => {
                *energy_cost /= stats.energy_efficiency;
                *buildup_duration /= stats.speed;
                *recover_duration /= stats.speed;
                *melee = melee.as_ref().cloned().map(|m| m.adjusted_by_stats(stats));
                *projectile = projectile.clone().adjusted_by_stats(stats);
                *projectile_speed *= stats.range;
            },
            Simple {
                ref mut energy_cost,
                combo_cost: _,
                ref mut buildup_duration,
                meta: _,
            } => {
                *energy_cost /= stats.energy_efficiency;
                *buildup_duration /= stats.speed;
            },
            Knock {
                ref mut energy_cost,
                ref mut buildup_duration,
                ref mut cast_duration,
                ref mut recover_duration,
                ref mut range,
                ori_modifier: _,
                meta: _,
            } => {
                *energy_cost /= stats.energy_efficiency;
                *buildup_duration /= stats.speed;
                *cast_duration /= stats.speed;
                *recover_duration /= stats.speed;
                *range *= stats.range;
            },
        }
        self
    }

    pub fn energy_cost(&self) -> f32 {
        use CharacterAbility::*;
        match self {
            BasicMelee { energy_cost, .. }
            | BasicRanged { energy_cost, .. }
            | RapidRanged { energy_cost, .. }
            | DashMelee { energy_cost, .. }
            | Roll { energy_cost, .. }
            | LeapExplosionShockwave { energy_cost, .. }
            | LeapMelee { energy_cost, .. }
            | LeapShockwave { energy_cost, .. }
            | ChargedMelee { energy_cost, .. }
            | ChargedRanged { energy_cost, .. }
            | Throw { energy_cost, .. }
            | TelekineticGrip { energy_cost, .. }
            | Shockwave { energy_cost, .. }
            | Explosion { energy_cost, .. }
            | GroundAoe { energy_cost, .. }
            | BasicAura { energy_cost, .. }
            | BasicBlock { energy_cost, .. }
            | SelfBuff { energy_cost, .. }
            | FinisherMelee { energy_cost, .. }
            | ComboMelee2 {
                energy_cost_per_strike: energy_cost,
                ..
            }
            | DiveMelee { energy_cost, .. }
            | RiposteMelee { energy_cost, .. }
            | RapidMelee { energy_cost, .. }
            | StaticAura { energy_cost, .. }
            | RegrowHead { energy_cost, .. }
            | LeapRanged { energy_cost, .. }
            | Knock { energy_cost, .. }
            | Simple { energy_cost, .. } => *energy_cost,
            BasicBeam { energy_drain, .. } => {
                if *energy_drain > f32::EPSILON {
                    1.0
                } else {
                    0.0
                }
            },
            Boost { .. }
            | GlideBoost { .. }
            | Blink { .. }
            | Music { .. }
            | BasicSummon { .. }
            | SpriteSummon { .. }
            | Transform { .. } => 0.0,
        }
    }

    #[expect(clippy::bool_to_int_with_if)]
    pub fn combo_cost(&self) -> u32 {
        use CharacterAbility::*;
        match self {
            BasicAura {
                scales_with_combo, ..
            } => {
                if *scales_with_combo {
                    1
                } else {
                    0
                }
            },
            FinisherMelee {
                minimum_combo: combo,
                ..
            }
            | RapidMelee {
                minimum_combo: combo,
                ..
            }
            | SelfBuff {
                combo_cost: combo, ..
            }
            | Simple {
                combo_cost: combo, ..
            } => *combo,
            Shockwave {
                minimum_combo: combo,
                ..
            } => combo.unwrap_or(0),
            BasicMelee { .. }
            | BasicRanged { .. }
            | RapidRanged { .. }
            | DashMelee { .. }
            | Roll { .. }
            | LeapExplosionShockwave { .. }
            | LeapMelee { .. }
            | LeapShockwave { .. }
            | Explosion { .. }
            | GroundAoe { .. }
            | ChargedMelee { .. }
            | ChargedRanged { .. }
            | Throw { .. }
            | TelekineticGrip { .. }
            | BasicBlock { .. }
            | ComboMelee2 { .. }
            | DiveMelee { .. }
            | RiposteMelee { .. }
            | BasicBeam { .. }
            | Boost { .. }
            | GlideBoost { .. }
            | Blink { .. }
            | Music { .. }
            | BasicSummon { .. }
            | SpriteSummon { .. }
            | Transform { .. }
            | StaticAura { .. }
            | RegrowHead { .. }
            | LeapRanged { .. }
            | Knock { .. } => 0,
        }
    }

    // TODO: Maybe consider making CharacterAbility a struct at some point?
    pub fn ability_meta(&self) -> AbilityMeta {
        use CharacterAbility::*;
        match self {
            BasicMelee { meta, .. }
            | BasicRanged { meta, .. }
            | RapidRanged { meta, .. }
            | DashMelee { meta, .. }
            | Roll { meta, .. }
            | LeapExplosionShockwave { meta, .. }
            | LeapMelee { meta, .. }
            | LeapShockwave { meta, .. }
            | ChargedMelee { meta, .. }
            | ChargedRanged { meta, .. }
            | Throw { meta, .. }
            | TelekineticGrip { meta, .. }
            | Shockwave { meta, .. }
            | Explosion { meta, .. }
            | GroundAoe { meta, .. }
            | BasicAura { meta, .. }
            | BasicBlock { meta, .. }
            | SelfBuff { meta, .. }
            | BasicBeam { meta, .. }
            | Boost { meta, .. }
            | GlideBoost { meta, .. }
            | ComboMelee2 { meta, .. }
            | Blink { meta, .. }
            | BasicSummon { meta, .. }
            | SpriteSummon { meta, .. }
            | FinisherMelee { meta, .. }
            | Music { meta, .. }
            | DiveMelee { meta, .. }
            | RiposteMelee { meta, .. }
            | RapidMelee { meta, .. }
            | Transform { meta, .. }
            | StaticAura { meta, .. }
            | RegrowHead { meta, .. }
            | LeapRanged { meta, .. }
            | Knock { meta, .. }
            | Simple { meta, .. } => *meta,
        }
    }

    #[must_use = "method returns new ability and doesn't mutate the original value"]
    pub fn adjusted_by_skills(mut self, skillset: &SkillSet, tool: Option<ToolKind>) -> Self {
        match tool {
            Some(ToolKind::Sceptre) => self.adjusted_by_sceptre_skills(skillset),
            Some(ToolKind::Pick) => self.adjusted_by_mining_skills(skillset),
            None | Some(_) => {},
        }
        self
    }

    /// BL-06 Q5 — dynamic capstone synergies: a capstone active scales off the
    /// rank of its sibling passive, read from the live SkillSet at
    /// ability-build time and keyed by the InnateAux ability id. Per-rank
    /// factors are balance placeholders, kept as code constants consistent
    /// with the existing SKILL_MODIFIERS convention.
    /// TODO: move the synergy factors (and SKILL_MODIFIERS) to RON so designers
    /// can tune them without a recompile.
    ///
    /// NOTE: every keyed capstone here is currently a `SelfBuff`; the scaling
    /// only applies to that variant. If a future synergy-keyed capstone is
    /// authored as another variant, the `debug_assert!` below flags the silent
    /// no-op so the synergy isn't quietly dropped.
    #[must_use = "method returns new ability and doesn't mutate the original value"]
    pub fn adjusted_by_class_synergy(mut self, skillset: &SkillSet, ability_id: &str) -> Self {
        use skills::{MageSkill, RogueSkill, Skill, WarriorSkill};
        let rank = |s: Skill| skillset.skill_level(s).unwrap_or(0) as f32;
        let scale = match ability_id {
            "class.warrior.onslaught" => {
                1.0 + 0.08 * rank(Skill::Warrior(WarriorSkill::BrutalEdge))
            },
            "class.mage.arcanemastery" => 1.0 + 0.06 * rank(Skill::Mage(MageSkill::FocusedMind)),
            "class.rogue.vanish" => 1.0 + 0.08 * rank(Skill::Rogue(RogueSkill::DeadlyPrecision)),
            _ => return self,
        };
        if let CharacterAbility::SelfBuff { buffs, .. } = &mut self {
            for b in buffs.iter_mut() {
                b.data.strength *= scale;
            }
        } else {
            debug_assert!(
                false,
                "class synergy for `{ability_id}` expects a SelfBuff ability; synergy dropped",
            );
        }
        self
    }

    fn adjusted_by_mining_skills(&mut self, skillset: &SkillSet) {
        use skills::MiningSkill::Speed;

        if let CharacterAbility::BasicMelee {
            buildup_duration,
            swing_duration,
            recover_duration,
            ..
        } = self
            && let Ok(level) = skillset.skill_level(Skill::Pick(Speed))
        {
            let modifiers = SKILL_MODIFIERS.mining_tree;

            let speed = modifiers.speed.powi(level.into());
            *buildup_duration /= speed;
            *swing_duration /= speed;
            *recover_duration /= speed;
        }
    }

    fn adjusted_by_sceptre_skills(&mut self, skillset: &SkillSet) {
        use skills::{SceptreSkill::*, Skill::Sceptre};

        match self {
            CharacterAbility::BasicBeam {
                damage,
                range,
                beam_duration,
                damage_effect,
                energy_regen,
                ..
            } => {
                let modifiers = SKILL_MODIFIERS.sceptre_tree.beam;
                if let Ok(level) = skillset.skill_level(Sceptre(LDamage)) {
                    *damage *= modifiers.damage.powi(level.into());
                }
                if let Ok(level) = skillset.skill_level(Sceptre(LRange)) {
                    let range_mod = modifiers.range.powi(level.into());
                    *range *= range_mod;
                    // Duration modified to keep velocity constant
                    *beam_duration *= range_mod as f64;
                }
                if let Ok(level) = skillset.skill_level(Sceptre(LRegen)) {
                    *energy_regen *= modifiers.energy_regen.powi(level.into());
                }
                if let (Ok(level), Some(CombatEffect::Lifesteal(lifesteal))) =
                    (skillset.skill_level(Sceptre(LLifesteal)), damage_effect)
                {
                    *lifesteal *= modifiers.lifesteal.powi(level.into());
                }
            },
            CharacterAbility::BasicAura {
                auras,
                range,
                energy_cost,
                specifier: Some(aura::Specifier::HealingAura),
                ..
            } => {
                let modifiers = SKILL_MODIFIERS.sceptre_tree.healing_aura;
                if let Ok(level) = skillset.skill_level(Sceptre(HHeal)) {
                    auras.iter_mut().for_each(|ref mut aura| {
                        aura.strength *= modifiers.strength.powi(level.into());
                    });
                }
                if let Ok(level) = skillset.skill_level(Sceptre(HDuration)) {
                    auras.iter_mut().for_each(|ref mut aura| {
                        if let Some(ref mut duration) = aura.duration {
                            *duration *= modifiers.duration.powi(level.into()) as f64;
                        }
                    });
                }
                if let Ok(level) = skillset.skill_level(Sceptre(HRange)) {
                    *range *= modifiers.range.powi(level.into());
                }
                if let Ok(level) = skillset.skill_level(Sceptre(HCost)) {
                    *energy_cost *= modifiers.energy_cost.powi(level.into());
                }
            },
            CharacterAbility::BasicAura {
                auras,
                range,
                energy_cost,
                specifier: Some(aura::Specifier::WardingAura),
                ..
            } => {
                let modifiers = SKILL_MODIFIERS.sceptre_tree.warding_aura;
                if let Ok(level) = skillset.skill_level(Sceptre(AStrength)) {
                    auras.iter_mut().for_each(|ref mut aura| {
                        aura.strength *= modifiers.strength.powi(level.into());
                    });
                }
                if let Ok(level) = skillset.skill_level(Sceptre(ADuration)) {
                    auras.iter_mut().for_each(|ref mut aura| {
                        if let Some(ref mut duration) = aura.duration {
                            *duration *= modifiers.duration.powi(level.into()) as f64;
                        }
                    });
                }
                if let Ok(level) = skillset.skill_level(Sceptre(ARange)) {
                    *range *= modifiers.range.powi(level.into());
                }
                if let Ok(level) = skillset.skill_level(Sceptre(ACost)) {
                    *energy_cost *= modifiers.energy_cost.powi(level.into());
                }
            },
            _ => {},
        }
    }
}

/// Small helper for #[serde(default)] booleans
fn default_true() -> bool { true }

#[derive(Debug)]
pub enum CharacterStateCreationError {
    MissingHandInfo,
    MissingItem,
    InvalidItemKind,
}

impl TryFrom<(&CharacterAbility, AbilityInfo, &JoinData<'_>)> for CharacterState {
    type Error = CharacterStateCreationError;

    fn try_from(
        (ability, ability_info, data): (&CharacterAbility, AbilityInfo, &JoinData),
    ) -> Result<Self, Self::Error> {
        Ok(match ability {
            CharacterAbility::BasicMelee {
                buildup_duration,
                swing_duration,
                hit_timing,
                recover_duration,
                melee_constructor,
                movement_modifier,
                ori_modifier,
                frontend_specifier,
                energy_cost: _,
                meta: _,
            } => CharacterState::BasicMelee(basic_melee::Data {
                static_data: basic_melee::StaticData {
                    buildup_duration: Duration::from_secs_f32(*buildup_duration),
                    swing_duration: Duration::from_secs_f32(*swing_duration),
                    hit_timing: hit_timing.clamp(0.0, 1.0),
                    recover_duration: Duration::from_secs_f32(*recover_duration),
                    melee_constructor: melee_constructor.clone(),
                    movement_modifier: *movement_modifier,
                    ori_modifier: *ori_modifier,
                    frontend_specifier: *frontend_specifier,
                    ability_info,
                },
                timer: Duration::default(),
                stage_section: StageSection::Buildup,
                exhausted: false,
                movement_modifier: movement_modifier.buildup,
                ori_modifier: ori_modifier.buildup,
            }),
            CharacterAbility::BasicRanged {
                buildup_duration,
                recover_duration,
                projectile,
                projectile_body,
                projectile_light,
                projectile_speed,
                vertical_angle_offset,
                energy_cost: _,
                num_projectiles,
                projectile_spread,
                auto_aim,
                movement_modifier,
                ori_modifier,
                marker,
                meta: _,
            } => CharacterState::BasicRanged(basic_ranged::Data {
                static_data: basic_ranged::StaticData {
                    buildup_duration: Duration::from_secs_f32(*buildup_duration),
                    recover_duration: Duration::from_secs_f32(*recover_duration),
                    projectile: projectile.clone(),
                    projectile_body: *projectile_body,
                    projectile_light: *projectile_light,
                    projectile_speed: *projectile_speed,
                    vertical_angle_offset: *vertical_angle_offset,
                    num_projectiles: *num_projectiles,
                    projectile_spread: *projectile_spread,
                    auto_aim: *auto_aim,
                    ability_info,
                    movement_modifier: *movement_modifier,
                    ori_modifier: *ori_modifier,
                    marker: *marker,
                },
                timer: Duration::default(),
                stage_section: StageSection::Buildup,
                exhausted: false,
                movement_modifier: movement_modifier.buildup,
                ori_modifier: ori_modifier.buildup,
            }),
            CharacterAbility::Boost {
                movement_duration,
                only_up,
                speed,
                max_exit_velocity,
                meta: _,
            } => CharacterState::Boost(boost::Data {
                static_data: boost::StaticData {
                    movement_duration: Duration::from_secs_f32(*movement_duration),
                    only_up: *only_up,
                    speed: *speed,
                    max_exit_velocity: *max_exit_velocity,
                    ability_info,
                },
                timer: Duration::default(),
            }),
            CharacterAbility::GlideBoost { booster, meta: _ } => {
                let scale = data.body.dimensions().z.sqrt();
                let mut glide_data = glide::Data::new(scale * 4.5, scale, *data.ori);
                glide_data.booster = Some(*booster);

                CharacterState::Glide(glide_data)
            },
            CharacterAbility::DashMelee {
                energy_cost: _,
                energy_drain,
                forward_speed,
                buildup_duration,
                charge_duration,
                swing_duration,
                recover_duration,
                melee_constructor,
                ori_modifier,
                auto_charge,
                charge_through,
                frontend_specifier,
                meta: _,
            } => CharacterState::DashMelee(dash_melee::Data {
                static_data: dash_melee::StaticData {
                    energy_drain: *energy_drain,
                    forward_speed: *forward_speed,
                    auto_charge: *auto_charge,
                    charge_through: *charge_through,
                    buildup_duration: Duration::from_secs_f32(*buildup_duration),
                    charge_duration: Duration::from_secs_f32(*charge_duration),
                    swing_duration: Duration::from_secs_f32(*swing_duration),
                    recover_duration: Duration::from_secs_f32(*recover_duration),
                    melee_constructor: melee_constructor.clone(),
                    ori_modifier: *ori_modifier,
                    frontend_specifier: *frontend_specifier,
                    ability_info,
                },
                auto_charge: false,
                timer: Duration::default(),
                stage_section: StageSection::Buildup,
            }),
            CharacterAbility::BasicBlock {
                buildup_duration,
                recover_duration,
                max_angle,
                block_strength,
                parry_window,
                energy_cost,
                energy_regen,
                can_hold,
                blocked_attacks,
                meta: _,
            } => CharacterState::BasicBlock(basic_block::Data {
                static_data: basic_block::StaticData {
                    buildup_duration: Duration::from_secs_f32(*buildup_duration),
                    recover_duration: Duration::from_secs_f32(*recover_duration),
                    max_angle: *max_angle,
                    block_strength: *block_strength,
                    parry_window: *parry_window,
                    energy_cost: *energy_cost,
                    energy_regen: *energy_regen,
                    can_hold: *can_hold,
                    blocked_attacks: *blocked_attacks,
                    ability_info,
                },
                timer: Duration::default(),
                stage_section: StageSection::Buildup,
                is_parry: false,
            }),
            CharacterAbility::Roll {
                energy_cost: _,
                buildup_duration,
                movement_duration,
                recover_duration,
                roll_strength,
                attack_immunities,
                was_cancel,
                meta: _,
            } => CharacterState::Roll(roll::Data {
                static_data: roll::StaticData {
                    buildup_duration: Duration::from_secs_f32(*buildup_duration),
                    movement_duration: Duration::from_secs_f32(*movement_duration),
                    recover_duration: Duration::from_secs_f32(*recover_duration),
                    roll_strength: *roll_strength,
                    attack_immunities: *attack_immunities,
                    was_cancel: *was_cancel,
                    ability_info,
                },
                timer: Duration::default(),
                stage_section: StageSection::Buildup,
                was_wielded: false, // false by default. utils might set it to true
                prev_aimed_dir: None,
                is_sneaking: false,
            }),
            CharacterAbility::ComboMelee2 {
                strikes,
                energy_cost_per_strike,
                specifier,
                auto_progress,
                meta: _,
            } => CharacterState::ComboMelee2(combo_melee2::Data {
                static_data: combo_melee2::StaticData {
                    strikes: strikes.iter().cloned().map(|s| s.to_duration()).collect(),
                    energy_cost_per_strike: *energy_cost_per_strike,
                    specifier: *specifier,
                    auto_progress: *auto_progress,
                    ability_info,
                },
                exhausted: false,
                start_next_strike: false,
                timer: Duration::default(),
                stage_section: StageSection::Buildup,
                completed_strikes: 0,
                movement_modifier: strikes.first().and_then(|s| s.movement_modifier.buildup),
                ori_modifier: strikes.first().and_then(|s| s.ori_modifier.buildup),
            }),
            CharacterAbility::LeapExplosionShockwave {
                energy_cost: _,
                buildup_duration,
                movement_duration,
                swing_duration,
                recover_duration,
                forward_leap_strength,
                vertical_leap_strength,
                explosion_damage,
                explosion_poise,
                explosion_knockback,
                explosion_radius,
                min_falloff,
                explosion_dodgeable,
                destroy_terrain,
                replace_terrain,
                eye_height,
                reagent,
                shockwave_damage,
                shockwave_poise,
                shockwave_knockback,
                shockwave_angle,
                shockwave_vertical_angle,
                shockwave_speed,
                shockwave_duration,
                shockwave_dodgeable,
                shockwave_damage_effect,
                shockwave_damage_kind,
                shockwave_specifier,
                move_efficiency,
                meta: _,
            } => CharacterState::LeapExplosionShockwave(leap_explosion_shockwave::Data {
                static_data: leap_explosion_shockwave::StaticData {
                    buildup_duration: Duration::from_secs_f32(*buildup_duration),
                    movement_duration: Duration::from_secs_f32(*movement_duration),
                    swing_duration: Duration::from_secs_f32(*swing_duration),
                    recover_duration: Duration::from_secs_f32(*recover_duration),
                    forward_leap_strength: *forward_leap_strength,
                    vertical_leap_strength: *vertical_leap_strength,
                    explosion_damage: *explosion_damage,
                    explosion_poise: *explosion_poise,
                    explosion_knockback: *explosion_knockback,
                    explosion_radius: *explosion_radius,
                    min_falloff: *min_falloff,
                    explosion_dodgeable: *explosion_dodgeable,
                    destroy_terrain: *destroy_terrain,
                    replace_terrain: *replace_terrain,
                    eye_height: *eye_height,
                    reagent: *reagent,
                    shockwave_damage: *shockwave_damage,
                    shockwave_poise: *shockwave_poise,
                    shockwave_knockback: *shockwave_knockback,
                    shockwave_angle: *shockwave_angle,
                    shockwave_vertical_angle: *shockwave_vertical_angle,
                    shockwave_speed: *shockwave_speed,
                    shockwave_duration: Duration::from_secs_f32(*shockwave_duration),
                    shockwave_dodgeable: *shockwave_dodgeable,
                    shockwave_damage_effect: shockwave_damage_effect.clone(),
                    shockwave_damage_kind: *shockwave_damage_kind,
                    shockwave_specifier: *shockwave_specifier,
                    move_efficiency: *move_efficiency,
                    ability_info,
                },
                timer: Duration::default(),
                stage_section: StageSection::Buildup,
                exhausted: false,
            }),
            CharacterAbility::LeapMelee {
                energy_cost: _,
                buildup_duration,
                movement_duration,
                swing_duration,
                recover_duration,
                melee_constructor,
                forward_leap_strength,
                vertical_leap_strength,
                specifier,
                meta: _,
            } => CharacterState::LeapMelee(leap_melee::Data {
                static_data: leap_melee::StaticData {
                    buildup_duration: Duration::from_secs_f32(*buildup_duration),
                    movement_duration: Duration::from_secs_f32(*movement_duration),
                    swing_duration: Duration::from_secs_f32(*swing_duration),
                    recover_duration: Duration::from_secs_f32(*recover_duration),
                    melee_constructor: melee_constructor.clone(),
                    forward_leap_strength: *forward_leap_strength,
                    vertical_leap_strength: *vertical_leap_strength,
                    ability_info,
                    specifier: *specifier,
                },
                timer: Duration::default(),
                stage_section: StageSection::Buildup,
                exhausted: false,
            }),
            CharacterAbility::LeapShockwave {
                energy_cost: _,
                buildup_duration,
                movement_duration,
                swing_duration,
                recover_duration,
                damage,
                poise_damage,
                knockback,
                shockwave_angle,
                shockwave_vertical_angle,
                shockwave_speed,
                shockwave_duration,
                dodgeable,
                move_efficiency,
                damage_kind,
                specifier,
                damage_effect,
                forward_leap_strength,
                vertical_leap_strength,
                meta: _,
            } => CharacterState::LeapShockwave(leap_shockwave::Data {
                static_data: leap_shockwave::StaticData {
                    buildup_duration: Duration::from_secs_f32(*buildup_duration),
                    movement_duration: Duration::from_secs_f32(*movement_duration),
                    swing_duration: Duration::from_secs_f32(*swing_duration),
                    recover_duration: Duration::from_secs_f32(*recover_duration),
                    damage: *damage,
                    poise_damage: *poise_damage,
                    knockback: *knockback,
                    shockwave_angle: *shockwave_angle,
                    shockwave_vertical_angle: *shockwave_vertical_angle,
                    shockwave_speed: *shockwave_speed,
                    shockwave_duration: Duration::from_secs_f32(*shockwave_duration),
                    dodgeable: *dodgeable,
                    move_efficiency: *move_efficiency,
                    damage_kind: *damage_kind,
                    specifier: *specifier,
                    damage_effect: damage_effect.clone(),
                    forward_leap_strength: *forward_leap_strength,
                    vertical_leap_strength: *vertical_leap_strength,
                    ability_info,
                },
                timer: Duration::default(),
                stage_section: StageSection::Buildup,
                exhausted: false,
            }),
            CharacterAbility::ChargedMelee {
                energy_cost,
                energy_drain,
                buildup_strike,
                charge_duration,
                swing_duration,
                hit_timing,
                recover_duration,
                melee_constructor,
                specifier,
                custom_combo,
                meta: _,
                movement_modifier,
                ori_modifier,
            } => CharacterState::ChargedMelee(charged_melee::Data {
                static_data: charged_melee::StaticData {
                    energy_cost: *energy_cost,
                    energy_drain: *energy_drain,
                    buildup_strike: buildup_strike
                        .as_ref()
                        .map(|(dur, strike)| (Duration::from_secs_f32(*dur), strike.clone())),
                    charge_duration: Duration::from_secs_f32(*charge_duration),
                    swing_duration: Duration::from_secs_f32(*swing_duration),
                    hit_timing: *hit_timing,
                    recover_duration: Duration::from_secs_f32(*recover_duration),
                    melee_constructor: melee_constructor.clone(),
                    ability_info,
                    specifier: *specifier,
                    custom_combo: custom_combo.clone(),
                    movement_modifier: *movement_modifier,
                    ori_modifier: *ori_modifier,
                },
                stage_section: if buildup_strike.is_some() {
                    StageSection::Buildup
                } else {
                    StageSection::Charge
                },
                timer: Duration::default(),
                exhausted: false,
                charge_amount: 0.0,
                movement_modifier: movement_modifier.buildup,
                ori_modifier: ori_modifier.buildup,
            }),
            CharacterAbility::ChargedRanged {
                energy_cost: _,
                energy_drain,
                idle_drain,
                projectile,
                buildup_duration,
                charge_duration,
                recover_duration,
                projectile_body,
                projectile_light,
                initial_projectile_speed,
                scaled_projectile_speed,
                projectile_spread,
                num_projectiles,
                marker,
                move_speed,
                meta: _,
            } => CharacterState::ChargedRanged(charged_ranged::Data {
                static_data: charged_ranged::StaticData {
                    buildup_duration: Duration::from_secs_f32(*buildup_duration),
                    charge_duration: Duration::from_secs_f32(*charge_duration),
                    recover_duration: Duration::from_secs_f32(*recover_duration),
                    energy_drain: *energy_drain,
                    idle_drain: *idle_drain,
                    projectile: projectile.clone(),
                    projectile_body: *projectile_body,
                    projectile_light: *projectile_light,
                    initial_projectile_speed: *initial_projectile_speed,
                    scaled_projectile_speed: *scaled_projectile_speed,
                    projectile_spread: *projectile_spread,
                    num_projectiles: *num_projectiles,
                    marker: *marker,
                    move_speed: *move_speed,
                    ability_info,
                },
                timer: Duration::default(),
                stage_section: StageSection::Buildup,
                exhausted: false,
            }),
            CharacterAbility::RapidRanged {
                initial_energy: _,
                energy_cost,
                buildup_duration,
                shoot_duration,
                recover_duration,
                options,
                projectile,
                projectile_body,
                projectile_light,
                projectile_speed,
                specifier,
                meta: _,
            } => CharacterState::RapidRanged(rapid_ranged::Data {
                static_data: rapid_ranged::StaticData {
                    buildup_duration: Duration::from_secs_f32(*buildup_duration),
                    shoot_duration: Duration::from_secs_f32(*shoot_duration),
                    recover_duration: Duration::from_secs_f32(*recover_duration),
                    energy_cost: *energy_cost,
                    options: *options,
                    projectile: projectile.clone(),
                    projectile_body: *projectile_body,
                    projectile_light: *projectile_light,
                    projectile_speed: *projectile_speed,
                    ability_info,
                    specifier: *specifier,
                },
                timer: Duration::default(),
                stage_section: StageSection::Buildup,
                projectiles_fired: 0,
                speed: 1.0,
            }),
            CharacterAbility::Throw {
                energy_cost: _,
                energy_drain,
                buildup_duration,
                charge_duration,
                throw_duration,
                recover_duration,
                projectile,
                projectile_light,
                projectile_dir,
                initial_projectile_speed,
                scaled_projectile_speed,
                damage_effect,
                move_speed,
                meta: _,
            } => {
                let hand_info = if let Some(hand_info) = ability_info.hand {
                    hand_info
                } else {
                    return Err(CharacterStateCreationError::MissingHandInfo);
                };

                let equip_slot = hand_info.to_equip_slot();

                let equipped_item =
                    if let Some(item) = data.inventory.and_then(|inv| inv.equipped(equip_slot)) {
                        item
                    } else {
                        return Err(CharacterStateCreationError::MissingItem);
                    };

                let item_hash = equipped_item.item_hash();

                let tool_kind = if let ItemKind::Tool(Tool { kind, .. }) = *equipped_item.kind() {
                    kind
                } else {
                    return Err(CharacterStateCreationError::InvalidItemKind);
                };

                CharacterState::Throw(throw::Data {
                    static_data: throw::StaticData {
                        buildup_duration: Duration::from_secs_f32(*buildup_duration),
                        charge_duration: Duration::from_secs_f32(*charge_duration),
                        throw_duration: Duration::from_secs_f32(*throw_duration),
                        recover_duration: Duration::from_secs_f32(*recover_duration),
                        energy_drain: *energy_drain,
                        projectile: projectile.clone(),
                        projectile_light: *projectile_light,
                        projectile_dir: *projectile_dir,
                        initial_projectile_speed: *initial_projectile_speed,
                        scaled_projectile_speed: *scaled_projectile_speed,
                        move_speed: *move_speed,
                        ability_info,
                        damage_effect: damage_effect.clone(),
                        equip_slot,
                        item_hash,
                        hand_info,
                        tool_kind,
                    },
                    timer: Duration::default(),
                    stage_section: StageSection::Buildup,
                    exhausted: false,
                })
            },
            CharacterAbility::TelekineticGrip {
                energy_cost: _,
                energy_drain,
                buildup_duration,
                charge_duration,
                place_threshold,
                recover_duration,
                range,
                tether_length,
                initial_projectile_speed,
                scaled_projectile_speed,
                projectile,
                move_speed,
                meta: _,
            } => CharacterState::TelekineticGrip(telekinetic_grip::Data {
                static_data: telekinetic_grip::StaticData {
                    buildup_duration: Duration::from_secs_f32(*buildup_duration),
                    charge_duration: Duration::from_secs_f32(*charge_duration),
                    place_threshold: Duration::from_secs_f32(*place_threshold),
                    recover_duration: Duration::from_secs_f32(*recover_duration),
                    energy_drain: *energy_drain,
                    range: *range,
                    tether_length: *tether_length,
                    initial_projectile_speed: *initial_projectile_speed,
                    scaled_projectile_speed: *scaled_projectile_speed,
                    projectile: projectile.clone(),
                    move_speed: *move_speed,
                    ability_info,
                },
                timer: Duration::default(),
                stage_section: StageSection::Buildup,
                item: None,
                thrown: false,
            }),
            CharacterAbility::Shockwave {
                energy_cost: _,
                buildup_duration,
                swing_duration,
                recover_duration,
                damage,
                poise_damage,
                knockback,
                shockwave_angle,
                shockwave_vertical_angle,
                shockwave_speed,
                shockwave_duration,
                dodgeable,
                move_efficiency,
                damage_kind,
                specifier,
                ori_rate,
                damage_effect,
                timing,
                emit_outcome,
                minimum_combo,
                combo_consumption,
                meta: _,
            } => CharacterState::Shockwave(shockwave::Data {
                static_data: shockwave::StaticData {
                    buildup_duration: Duration::from_secs_f32(*buildup_duration),
                    swing_duration: Duration::from_secs_f32(*swing_duration),
                    recover_duration: Duration::from_secs_f32(*recover_duration),
                    damage: *damage,
                    poise_damage: *poise_damage,
                    knockback: *knockback,
                    shockwave_angle: *shockwave_angle,
                    shockwave_vertical_angle: *shockwave_vertical_angle,
                    shockwave_speed: *shockwave_speed,
                    shockwave_duration: Duration::from_secs_f32(*shockwave_duration),
                    dodgeable: *dodgeable,
                    move_efficiency: *move_efficiency,
                    damage_effect: damage_effect.clone(),
                    ability_info,
                    damage_kind: *damage_kind,
                    specifier: *specifier,
                    ori_rate: *ori_rate,
                    timing: *timing,
                    emit_outcome: *emit_outcome,
                    minimum_combo: *minimum_combo,
                    combo_on_use: data.combo.map_or(0, |c| c.counter()),
                    combo_consumption: *combo_consumption,
                },
                timer: Duration::default(),
                stage_section: StageSection::Buildup,
            }),
            CharacterAbility::Explosion {
                energy_cost: _,
                buildup_duration,
                action_duration,
                recover_duration,
                damage,
                poise,
                knockback,
                radius,
                min_falloff,
                dodgeable,
                destroy_terrain,
                replace_terrain,
                eye_height,
                reagent,
                movement_modifier,
                ori_modifier,
                meta: _,
            } => CharacterState::Explosion(explosion::Data {
                static_data: explosion::StaticData {
                    buildup_duration: Duration::from_secs_f32(*buildup_duration),
                    action_duration: Duration::from_secs_f32(*action_duration),
                    recover_duration: Duration::from_secs_f32(*recover_duration),
                    damage: *damage,
                    poise: *poise,
                    knockback: *knockback,
                    radius: *radius,
                    min_falloff: *min_falloff,
                    dodgeable: *dodgeable,
                    destroy_terrain: *destroy_terrain,
                    replace_terrain: *replace_terrain,
                    eye_height: *eye_height,
                    reagent: *reagent,
                    movement_modifier: *movement_modifier,
                    ori_modifier: *ori_modifier,
                    ability_info,
                },
                timer: Duration::default(),
                stage_section: StageSection::Buildup,
                movement_modifier: movement_modifier.buildup,
                ori_modifier: ori_modifier.buildup,
            }),
            CharacterAbility::GroundAoe {
                energy_cost: _,
                buildup_duration,
                delay,
                recover_duration,
                max_range,
                radius,
                min_falloff,
                damage,
                poise,
                knockback,
                dodgeable,
                reagent,
                rooted_cast,
                meta: _,
            } => CharacterState::GroundAoe(ground_aoe::Data {
                static_data: ground_aoe::StaticData {
                    buildup_duration: Duration::from_secs_f32(*buildup_duration),
                    delay: Duration::from_secs_f32(*delay),
                    recover_duration: Duration::from_secs_f32(*recover_duration),
                    max_range: *max_range,
                    radius: *radius,
                    min_falloff: *min_falloff,
                    damage: *damage,
                    poise: *poise,
                    knockback: *knockback,
                    dodgeable: *dodgeable,
                    reagent: *reagent,
                    rooted_cast: *rooted_cast,
                    ability_info,
                },
                timer: Duration::default(),
                stage_section: StageSection::Buildup,
                target_pos: None,
            }),
            CharacterAbility::BasicBeam {
                buildup_duration,
                recover_duration,
                beam_duration,
                damage,
                tick_rate,
                range,
                dodgeable,
                blockable,
                max_angle,
                damage_effect,
                energy_regen,
                energy_drain,
                move_efficiency,
                ori_rate,
                specifier,
                meta: _,
            } => CharacterState::BasicBeam(basic_beam::Data {
                static_data: basic_beam::StaticData {
                    buildup_duration: Duration::from_secs_f32(*buildup_duration),
                    recover_duration: Duration::from_secs_f32(*recover_duration),
                    beam_duration: Secs(*beam_duration),
                    damage: *damage,
                    tick_rate: *tick_rate,
                    range: *range,
                    dodgeable: *dodgeable,
                    blockable: *blockable,
                    end_radius: max_angle.to_radians().tan() * *range,
                    damage_effect: damage_effect.clone(),
                    energy_regen: *energy_regen,
                    energy_drain: *energy_drain,
                    ability_info,
                    move_efficiency: *move_efficiency,
                    ori_rate: *ori_rate,
                    specifier: *specifier,
                },
                timer: Duration::default(),
                stage_section: StageSection::Buildup,
                aim_dir: data.ori.look_dir(),
                beam_offset: data.pos.0,
            }),
            CharacterAbility::BasicAura {
                buildup_duration,
                cast_duration,
                recover_duration,
                targets,
                auras,
                tiered_health_effects,
                aura_duration,
                range,
                energy_cost: _,
                scales_with_combo,
                specifier,
                meta: _,
            } => CharacterState::BasicAura(basic_aura::Data {
                static_data: basic_aura::StaticData {
                    buildup_duration: Duration::from_secs_f32(*buildup_duration),
                    cast_duration: Duration::from_secs_f32(*cast_duration),
                    recover_duration: Duration::from_secs_f32(*recover_duration),
                    targets: *targets,
                    auras: auras.clone(),
                    tiered_health_effects: tiered_health_effects.clone(),
                    aura_duration: *aura_duration,
                    range: *range,
                    ability_info,
                    scales_with_combo: *scales_with_combo,
                    combo_at_cast: data.combo.map_or(0, |c| c.counter()),
                    specifier: *specifier,
                },
                timer: Duration::default(),
                stage_section: StageSection::Buildup,
            }),
            CharacterAbility::StaticAura {
                buildup_duration,
                cast_duration,
                recover_duration,
                targets,
                auras,
                aura_duration,
                range,
                energy_cost: _,
                sprite_info,
                meta: _,
            } => CharacterState::StaticAura(static_aura::Data {
                static_data: static_aura::StaticData {
                    buildup_duration: Duration::from_secs_f32(*buildup_duration),
                    cast_duration: Duration::from_secs_f32(*cast_duration),
                    recover_duration: Duration::from_secs_f32(*recover_duration),
                    targets: *targets,
                    auras: auras.clone(),
                    aura_duration: *aura_duration,
                    range: *range,
                    ability_info,
                    sprite_info: *sprite_info,
                },
                timer: Duration::default(),
                stage_section: StageSection::Buildup,
                achieved_radius: sprite_info.map(|si| si.summon_distance.0.floor() as i32 - 1),
            }),
            CharacterAbility::Blink {
                buildup_duration,
                recover_duration,
                max_range,
                frontend_specifier,
                meta: _,
            } => CharacterState::Blink(blink::Data {
                static_data: blink::StaticData {
                    buildup_duration: Duration::from_secs_f32(*buildup_duration),
                    recover_duration: Duration::from_secs_f32(*recover_duration),
                    max_range: *max_range,
                    frontend_specifier: *frontend_specifier,
                    ability_info,
                },
                timer: Duration::default(),
                stage_section: StageSection::Buildup,
            }),
            CharacterAbility::BasicSummon {
                buildup_duration,
                cast_duration,
                recover_duration,
                summon_info,
                movement_modifier,
                ori_modifier,
                meta: _,
            } => CharacterState::BasicSummon(basic_summon::Data {
                static_data: basic_summon::StaticData {
                    buildup_duration: Duration::from_secs_f32(*buildup_duration),
                    cast_duration: Duration::from_secs_f32(*cast_duration),
                    recover_duration: Duration::from_secs_f32(*recover_duration),
                    summon_info: summon_info.clone(),
                    movement_modifier: *movement_modifier,
                    ori_modifier: *ori_modifier,
                    ability_info,
                },
                summon_count: 0,
                timer: Duration::default(),
                stage_section: StageSection::Buildup,
                movement_modifier: movement_modifier.buildup,
                ori_modifier: ori_modifier.buildup,
            }),
            CharacterAbility::SelfBuff {
                buildup_duration,
                cast_duration,
                recover_duration,
                buffs,
                use_raw_buff_strength: _,
                buff_cat,
                energy_cost: _,
                combo_cost,
                combo_scaling,
                enforced_limit,
                meta: _,
                specifier,
            } => CharacterState::SelfBuff(self_buff::Data {
                static_data: self_buff::StaticData {
                    buildup_duration: Duration::from_secs_f32(*buildup_duration),
                    cast_duration: Duration::from_secs_f32(*cast_duration),
                    recover_duration: Duration::from_secs_f32(*recover_duration),
                    buffs: buffs.clone(),
                    buff_cat: buff_cat.clone(),
                    combo_cost: *combo_cost,
                    combo_scaling: *combo_scaling,
                    combo_on_use: data.combo.map_or(0, |c| c.counter()),
                    enforced_limit: *enforced_limit,
                    ability_info,
                    specifier: *specifier,
                },
                timer: Duration::default(),
                stage_section: StageSection::Buildup,
            }),
            CharacterAbility::SpriteSummon {
                buildup_duration,
                cast_duration,
                recover_duration,
                sprite,
                del_timeout,
                summon_distance,
                sparseness,
                angle,
                anchor,
                move_efficiency,
                ori_modifier,
                meta: _,
            } => CharacterState::SpriteSummon(sprite_summon::Data {
                static_data: sprite_summon::StaticData {
                    buildup_duration: Duration::from_secs_f32(*buildup_duration),
                    cast_duration: Duration::from_secs_f32(*cast_duration),
                    recover_duration: Duration::from_secs_f32(*recover_duration),
                    sprite: *sprite,
                    del_timeout: *del_timeout,
                    summon_distance: *summon_distance,
                    sparseness: *sparseness,
                    angle: *angle,
                    anchor: *anchor,
                    move_efficiency: *move_efficiency,
                    ori_modifier: *ori_modifier,
                    ability_info,
                },
                timer: Duration::default(),
                stage_section: StageSection::Buildup,
                achieved_radius: summon_distance.0.floor() as i32 - 1,
            }),
            CharacterAbility::Knock {
                energy_cost: _,
                buildup_duration,
                cast_duration,
                recover_duration,
                range,
                ori_modifier,
                meta: _,
            } => CharacterState::Knock(knock::Data {
                static_data: knock::StaticData {
                    buildup_duration: Duration::from_secs_f32(*buildup_duration),
                    cast_duration: Duration::from_secs_f32(*cast_duration),
                    recover_duration: Duration::from_secs_f32(*recover_duration),
                    range: *range,
                    ori_modifier: *ori_modifier,
                    ability_info,
                },
                timer: Duration::default(),
                stage_section: StageSection::Buildup,
                target_pos: None,
            }),
            CharacterAbility::Music {
                play_duration,
                ori_modifier,
                meta: _,
            } => CharacterState::Music(music::Data {
                static_data: music::StaticData {
                    play_duration: Duration::from_secs_f32(*play_duration),
                    ori_modifier: *ori_modifier,
                    ability_info,
                },
                timer: Duration::default(),
                stage_section: StageSection::Action,
                exhausted: false,
            }),
            CharacterAbility::FinisherMelee {
                energy_cost: _,
                buildup_duration,
                swing_duration,
                recover_duration,
                melee_constructor,
                minimum_combo,
                scaling,
                combo_consumption,
                meta: _,
            } => CharacterState::FinisherMelee(finisher_melee::Data {
                static_data: finisher_melee::StaticData {
                    buildup_duration: Duration::from_secs_f32(*buildup_duration),
                    swing_duration: Duration::from_secs_f32(*swing_duration),
                    recover_duration: Duration::from_secs_f32(*recover_duration),
                    melee_constructor: melee_constructor.clone(),
                    scaling: *scaling,
                    minimum_combo: *minimum_combo,
                    combo_on_use: data.combo.map_or(0, |c| c.counter()),
                    combo_consumption: *combo_consumption,
                    ability_info,
                },
                timer: Duration::default(),
                stage_section: StageSection::Buildup,
                exhausted: false,
            }),
            CharacterAbility::DiveMelee {
                buildup_duration,
                movement_duration,
                swing_duration,
                recover_duration,
                melee_constructor,
                energy_cost: _,
                vertical_speed,
                max_scaling,
                meta: _,
            } => CharacterState::DiveMelee(dive_melee::Data {
                static_data: dive_melee::StaticData {
                    buildup_duration: buildup_duration.map(Duration::from_secs_f32),
                    movement_duration: Duration::from_secs_f32(*movement_duration),
                    swing_duration: Duration::from_secs_f32(*swing_duration),
                    recover_duration: Duration::from_secs_f32(*recover_duration),
                    vertical_speed: *vertical_speed,
                    melee_constructor: melee_constructor.clone(),
                    max_scaling: *max_scaling,
                    ability_info,
                },
                timer: Duration::default(),
                stage_section: if data.physics.on_ground.is_none() || buildup_duration.is_none() {
                    StageSection::Movement
                } else {
                    StageSection::Buildup
                },
                exhausted: false,
                max_vertical_speed: 0.0,
            }),
            CharacterAbility::RiposteMelee {
                energy_cost: _,
                buildup_duration,
                swing_duration,
                recover_duration,
                whiffed_recover_duration,
                block_strength,
                melee_constructor,
                meta: _,
            } => CharacterState::RiposteMelee(riposte_melee::Data {
                static_data: riposte_melee::StaticData {
                    buildup_duration: Duration::from_secs_f32(*buildup_duration),
                    swing_duration: Duration::from_secs_f32(*swing_duration),
                    recover_duration: Duration::from_secs_f32(*recover_duration),
                    whiffed_recover_duration: Duration::from_secs_f32(*whiffed_recover_duration),
                    block_strength: *block_strength,
                    melee_constructor: melee_constructor.clone(),
                    ability_info,
                },
                timer: Duration::default(),
                stage_section: StageSection::Buildup,
                exhausted: false,
                whiffed: true,
            }),
            CharacterAbility::RapidMelee {
                buildup_duration,
                swing_duration,
                recover_duration,
                melee_constructor,
                energy_cost,
                max_strikes,
                move_modifier,
                ori_modifier,
                minimum_combo,
                frontend_specifier,
                meta: _,
            } => CharacterState::RapidMelee(rapid_melee::Data {
                static_data: rapid_melee::StaticData {
                    buildup_duration: Duration::from_secs_f32(*buildup_duration),
                    swing_duration: Duration::from_secs_f32(*swing_duration),
                    recover_duration: Duration::from_secs_f32(*recover_duration),
                    melee_constructor: melee_constructor.clone(),
                    energy_cost: *energy_cost,
                    max_strikes: *max_strikes,
                    move_modifier: *move_modifier,
                    ori_modifier: *ori_modifier,
                    minimum_combo: *minimum_combo,
                    frontend_specifier: *frontend_specifier,
                    ability_info,
                },
                timer: Duration::default(),
                current_strike: 1,
                stage_section: StageSection::Buildup,
                exhausted: false,
            }),
            CharacterAbility::Transform {
                buildup_duration,
                recover_duration,
                target,
                specifier,
                allow_players,
                meta: _,
            } => CharacterState::Transform(transform::Data {
                static_data: transform::StaticData {
                    buildup_duration: Duration::from_secs_f32(*buildup_duration),
                    recover_duration: Duration::from_secs_f32(*recover_duration),
                    specifier: *specifier,
                    allow_players: *allow_players,
                    target: target.to_owned(),
                    ability_info,
                },
                timer: Duration::default(),
                stage_section: StageSection::Buildup,
            }),
            CharacterAbility::RegrowHead {
                buildup_duration,
                recover_duration,
                energy_cost,
                specifier,
                meta: _,
            } => CharacterState::RegrowHead(regrow_head::Data {
                static_data: regrow_head::StaticData {
                    buildup_duration: Duration::from_secs_f32(*buildup_duration),
                    recover_duration: Duration::from_secs_f32(*recover_duration),
                    specifier: *specifier,
                    energy_cost: *energy_cost,
                    ability_info,
                },
                timer: Duration::default(),
                stage_section: StageSection::Buildup,
            }),
            CharacterAbility::LeapRanged {
                energy_cost: _,
                buildup_duration,
                buildup_melee_timing,
                movement_duration,
                movement_ranged_timing,
                land_timeout,
                recover_duration,
                melee,
                melee_required,
                projectile,
                projectile_body,
                projectile_light,
                projectile_speed,
                horiz_leap_strength,
                vert_leap_strength,
                meta: _,
            } => CharacterState::LeapRanged(leap_ranged::Data {
                static_data: leap_ranged::StaticData {
                    buildup_duration: Duration::from_secs_f32(*buildup_duration),
                    buildup_melee_timing: *buildup_melee_timing,
                    movement_duration: Duration::from_secs_f32(*movement_duration),
                    movement_ranged_timing: *movement_ranged_timing,
                    land_timeout: Duration::from_secs_f32(*land_timeout),
                    recover_duration: Duration::from_secs_f32(*recover_duration),
                    melee: melee.clone(),
                    melee_required: *melee_required,
                    projectile: projectile.clone(),
                    projectile_body: *projectile_body,
                    projectile_light: *projectile_light,
                    projectile_speed: *projectile_speed,
                    horiz_leap_strength: *horiz_leap_strength,
                    vert_leap_strength: *vert_leap_strength,
                    ability_info,
                },
                timer: Duration::default(),
                stage_section: StageSection::Buildup,
                melee_done: false,
                ranged_done: false,
            }),
            CharacterAbility::Simple {
                energy_cost: _,
                combo_cost: _,
                buildup_duration,
                meta: _,
            } => CharacterState::Simple(simple::Data {
                static_data: simple::StaticData {
                    buildup_duration: Duration::from_secs_f32(*buildup_duration),
                    ability_info,
                },
                timer: Duration::default(),
                stage_section: StageSection::Buildup,
            }),
        })
    }
}

/// What FORM a spell's effect takes (magic-system-v2 spec §1.2). Asset-only
/// metadata: drives UI grouping, class gating, future resistances. `None` for
/// abilities outside the school grid (Ki disciplines, raw psionic talents).
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum School {
    Abjuration,
    Conjuration,
    Divination,
    Enchantment,
    Evocation,
    Illusion,
    Necromancy,
    Transmutation,
    /// Time / gravity / mass / fate (our Dunamancy-analog; Prism-shard
    /// powered).
    Axiomancy,
    /// Blood-fuelled, self-corrupting magic (forbidden practice).
    Hemomancy,
}

/// The Axiomancy subschools (our Dunamancy-analog branches). Only meaningful
/// when `school == Axiomancy`; pairs with `form` (the classic school the effect
/// physically takes) as the composite tag `Axiomancy(Subschool · Form)`.
/// Asset-only metadata (magic-system-v2 §1.2; content-adaptation §4.1). IP:
/// coined — the source "Chronurgy/Graviturgy" names are denylisted.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum AxiomSub {
    /// Time / fate (Chronurgy-analog).
    Chronomancy,
    /// Gravity / mass (Graviturgy-analog).
    Gravimancy,
}

/// Where a spell's energy COMES FROM (magic-system-v2 spec §1.1). The fuel,
/// independent of the school (form). Ki and Psionic abilities live here with
/// `school: None`.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum MagicSource {
    /// The Veil — arcane study, bloodline, pact, or art.
    Arcane,
    /// The gods' channel through the Veil — faith and oaths.
    Divine,
    /// The Song still singing in the world — nature and elements, shaped by
    /// the Primordials.
    Primordial,
    /// Leakage from the Beyond — the mind as an unlicensed gate.
    Psionic,
    /// The Song flowing through a living body — discipline and ki.
    Ki,
}

bitflags::bitflags! {
    /// Per-`MagicSource` castable-cores bitset. Build/query via
    /// [`MagicSourceMask::for_source`]/[`MagicSourceMask::allows`], never by
    /// constructing bits directly. `Default` is deliberately empty
    /// (non-permissive); permissiveness is an explicit opt-in via
    /// `MagicSourceMask::all()` at the call site, so a missing narrowing can
    /// never silently read as "can cast everything".
    #[derive(Copy, Clone, Debug, PartialEq, Eq, Default, Serialize, Deserialize)]
    pub struct MagicSourceMask: u8 {
        const ARCANE     = 1 << 0;
        const DIVINE     = 1 << 1;
        const PRIMORDIAL = 1 << 2;
        const PSIONIC    = 1 << 3;
        const KI         = 1 << 4;
    }
}

impl MagicSourceMask {
    /// The bit covering `source`. Exhaustive match, deliberately with no
    /// `_ =>` arm: a new `MagicSource` variant added without a
    /// corresponding bit here fails the build instead of silently resolving
    /// to an empty mask.
    pub fn for_source(source: MagicSource) -> Self {
        match source {
            MagicSource::Arcane => Self::ARCANE,
            MagicSource::Divine => Self::DIVINE,
            MagicSource::Primordial => Self::PRIMORDIAL,
            MagicSource::Psionic => Self::PSIONIC,
            MagicSource::Ki => Self::KI,
        }
    }

    /// Does this mask allow casting from `source`?
    pub fn allows(self, source: MagicSource) -> bool { self.contains(Self::for_source(source)) }
}

#[derive(Copy, Clone, Debug, PartialEq, Serialize, Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct AbilityMeta {
    #[serde(default)]
    pub capabilities: Capability,
    #[serde(default)]
    /// This is an event that gets emitted when the ability is first activated
    pub init_event: Option<AbilityInitEvent>,
    // TODO: Evaluate if we want this to be a vec if we need more? Would lose copy though...
    pub init_event2: Option<AbilityInitEvent>,
    #[serde(default)]
    pub requirements: AbilityRequirements,
    /// Adjusts stats of ability when activated based on context.
    // If we ever add more, I guess change to a vec? Or maybe just an array if we want to keep
    // AbilityMeta small?
    pub contextual_stats: Option<StatAdj>,
    /// If provided, multiplies the precision power from armor for this ability
    pub precision_power_mult: Option<f32>,
    /// School this ability belongs to, if it is a spell. For the meta-schools
    /// (Axiomancy/Hemomancy) the underlying classic school is carried in
    /// `form`.
    #[serde(default)]
    pub school: Option<School>,
    /// For meta-school spells, the classic school the effect physically takes —
    /// e.g. `Axiomancy(Gravimancy · Evocation)` ⇒ `form = Evocation`,
    /// `Hemomancy(Necromancy)` ⇒ `form = Necromancy`. `None` for the classic
    /// spells. (magic-system-v2 §1.2; content-adaptation §4.1)
    #[serde(default)]
    pub form: Option<School>,
    /// Axiomancy subschool, when `school == Axiomancy`. `None` otherwise.
    #[serde(default)]
    pub subschool: Option<AxiomSub>,
    /// Magic source (fuel) this ability draws on, if any.
    #[serde(default)]
    pub source: Option<MagicSource>,
    /// Per-ability cooldown in seconds, gated in `handle_ability`.
    #[serde(default)]
    pub cooldown: Option<f32>,
    /// Per-ability HP cost — the Hemomancy "blood price" (M4 / ENG-C1). When
    /// set, casting spends this much of the caster's own HP. Normal play keeps
    /// a 1-HP floor (the cast is refused below `cost + 1`); see
    /// `states::utils::hp_cost_affordable`.
    #[serde(default)]
    pub hp_cost: Option<f32>,
}

impl StatAdj {
    pub fn equivalent_stats(&self, data: &JoinData) -> Stats {
        let mut stats = Stats::one();
        let add = match self.context {
            StatContext::PoiseResilience(base) => {
                let poise_res = combat::compute_poise_resilience(data.inventory, data.msm);
                poise_res.unwrap_or(0.0) / base.max(0.1)
            },
            StatContext::Stealth(base) => {
                let stealth = combat::compute_stealth(data.inventory, data.msm);
                stealth / base.max(0.1)
            },
        };
        match self.field {
            StatField::EffectPower => {
                stats.effect_power += add;
            },
            StatField::BuffStrength => {
                stats.buff_strength += add;
            },
            StatField::Power => {
                stats.power += add;
            },
        }
        stats
    }
}

#[derive(Copy, Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct StatAdj {
    /// If this much of the stat is achieved, 1.0 will be added to the affected
    /// stat
    pub context: StatContext,
    pub field: StatField,
}

#[derive(Copy, Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum StatContext {
    PoiseResilience(f32),
    Stealth(f32),
}

#[derive(Copy, Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum StatField {
    EffectPower,
    BuffStrength,
    Power,
}

#[derive(Copy, Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum AbilityReqItem {
    Firedrop,
    PoisonClot,
    GelidGel,
    LevinDust,
}

impl AbilityReqItem {
    pub fn item_def_id(&self) -> ItemDefinitionIdOwned {
        match self {
            Self::Firedrop => {
                ItemDefinitionIdOwned::Simple(String::from("common.items.consumable.firedrop"))
            },
            Self::PoisonClot => {
                ItemDefinitionIdOwned::Simple(String::from("common.items.consumable.poison_clot"))
            },
            Self::GelidGel => {
                ItemDefinitionIdOwned::Simple(String::from("common.items.consumable.gelid_gel"))
            },
            Self::LevinDust => {
                ItemDefinitionIdOwned::Simple(String::from("common.items.consumable.levin_dust"))
            },
        }
    }
}

// TODO: Later move over things like energy and combo into here
#[derive(Copy, Clone, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct AbilityRequirements {
    pub stance: Option<Stance>,
    pub item: Option<AbilityReqItem>,
    /// Whether this ability only exists while PROJECT ORACLE is live.
    /// Greyed-out in every ability picker and refused server-side when it is
    /// not — see `common::resources::OracleLive` and
    /// `CharacterAbility::requirements_paid`. Generic and reusable by any
    /// future ability; not specific to any one spell.
    #[serde(default)]
    pub oracle: bool,
    /// Minimum derived character level (`SkillSet::character_level`) to use
    /// this ability. Greyed out in every picker and refused server-side below
    /// it. Generic and reusable by any ability — the ability-side twin of
    /// `ItemRequirements.min_level`.
    #[serde(default)]
    pub min_level: Option<u16>,
}

impl AbilityRequirements {
    pub fn requirements_met(
        &self,
        stance: Option<&Stance>,
        inv: Option<&Inventory>,
        oracle_live: bool,
        character_level: u16,
    ) -> bool {
        let AbilityRequirements {
            stance: req_stance,
            item,
            oracle,
            min_level,
        } = self;
        let stance_met = req_stance
            .is_none_or(|req_stance| stance.is_some_and(|char_stance| req_stance == *char_stance));
        let item_met = item.is_none_or(|item| {
            inv.is_some_and(|inv| {
                inv.get_slot_of_item_by_def_id(&item.item_def_id())
                    .is_some()
            })
        });
        let oracle_met = !oracle || oracle_live;
        let level_met = min_level.is_none_or(|l| character_level >= l);
        stance_met && item_met && oracle_met && level_met
    }
}

bitflags::bitflags! {
    #[derive(Copy, Clone, Debug, PartialEq, Eq, Default, Serialize, Deserialize)]
    // If more are ever needed, first check if any not used anymore, as some were only used in intermediary stages so may be free
    pub struct Capability: u8 {
        // The ability will parry all blockable attacks in the buildup portion
        const PARRIES             = 0b00000001;
        // Allows blocking to interrupt the ability at any point
        const BLOCK_INTERRUPT     = 0b00000010;
        // The ability will block melee attacks in the buildup portion
        const BLOCKS              = 0b00000100;
        // When in the ability, an entity only receives half as much poise damage
        const POISE_RESISTANT     = 0b00001000;
        // WHen in the ability, an entity only receives half as much knockback
        const KNOCKBACK_RESISTANT = 0b00010000;
        // The ability will parry melee attacks in the buildup portion
        const PARRIES_MELEE       = 0b00100000;
    }
}

#[derive(
    Copy, Clone, Debug, PartialEq, Eq, Serialize, Deserialize, Hash, PartialOrd, Ord, Default,
)]
pub enum Stance {
    #[default]
    None,
    Sword(SwordStance),
    Bow(BowStance),
}

impl Stance {
    pub fn pseudo_ability_id(&self) -> &str {
        match self {
            Stance::Sword(SwordStance::Heavy) => "veloren.core.pseudo_abilities.sword.heavy_stance",
            Stance::Sword(SwordStance::Agile) => "veloren.core.pseudo_abilities.sword.agile_stance",
            Stance::Sword(SwordStance::Defensive) => {
                "veloren.core.pseudo_abilities.sword.defensive_stance"
            },
            Stance::Sword(SwordStance::Crippling) => {
                "veloren.core.pseudo_abilities.sword.crippling_stance"
            },
            Stance::Sword(SwordStance::Cleaving) => {
                "veloren.core.pseudo_abilities.sword.cleaving_stance"
            },
            Stance::Bow(BowStance::Barrage) => "common.abilities.bow.barrage",
            Stance::Bow(BowStance::Hawkstrike) => "common.abilities.bow.hawkstrike",
            Stance::Bow(BowStance::Heartseeker) => "common.abilities.bow.heartseeker",
            Stance::None => "veloren.core.pseudo_abilities.no_stance",
        }
    }
}

#[derive(Copy, Clone, Debug, PartialEq, Eq, Serialize, Deserialize, Hash, PartialOrd, Ord)]
pub enum SwordStance {
    Crippling,
    Cleaving,
    Defensive,
    Heavy,
    Agile,
}

#[derive(Copy, Clone, Debug, PartialEq, Eq, Serialize, Deserialize, Hash, PartialOrd, Ord)]
pub enum BowStance {
    Barrage,
    Heartseeker,
    Hawkstrike,
}

#[derive(Copy, Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum AbilityInitEvent {
    EnterStance(Stance),
    GainBuff {
        kind: buff::BuffKind,
        strength: f32,
        duration: Option<Secs>,
    },
    RemoveBuff(BuffKind),
}

impl Component for Stance {
    type Storage = DerefFlaggedStorage<Self, specs::VecStorage<Self>>;
}

#[cfg(test)]
mod ability_cooldown_tests {
    use super::*;
    use crate::resources::Time;

    #[test]
    fn fresh_component_is_ready() {
        let cds = AbilityCooldowns::default();
        assert!(cds.is_ready("common.abilities.spells.ruin.shatterburst", Time(0.0)));
    }

    #[test]
    fn set_blocks_until_ready_time() {
        let mut cds = AbilityCooldowns::default();
        cds.set("a", Time(10.0), 30.0);
        assert!(!cds.is_ready("a", Time(10.0)));
        assert!(!cds.is_ready("a", Time(39.9)));
        assert!(cds.is_ready("a", Time(40.0)));
        assert!(cds.is_ready("b", Time(10.0)));
    }

    #[test]
    fn set_prunes_expired_entries() {
        let mut cds = AbilityCooldowns::default();
        cds.set("a", Time(0.0), 5.0);
        // "a" became ready at t=5; setting "b" at t=100 prunes it
        cds.set("b", Time(100.0), 5.0);
        assert_eq!(cds.0.len(), 1);
        assert!(cds.ready_at("b").is_some());
    }
}

#[cfg(test)]
mod ability_meta_tag_tests {
    use super::*;

    // M1 (ENG-A1): the composite meta-school tag. Axiomancy(Subschool · Form) and
    // Hemomancy(Form) need `form` + `subschool` on AbilityMeta.
    // `deny_unknown_fields` means the RON must be accepted explicitly; the 542
    // classic spells leave both None.
    #[test]
    fn meta_parses_form_and_subschool() {
        // a gravity attack: Axiomancy(Gravimancy · Evocation)
        let meta: AbilityMeta = ron::from_str(
            "(school: Some(Axiomancy), subschool: Some(Gravimancy), form: Some(Evocation))",
        )
        .expect("AbilityMeta with form + subschool must deserialize");
        assert_eq!(meta.school, Some(School::Axiomancy));
        assert_eq!(meta.subschool, Some(AxiomSub::Gravimancy));
        assert_eq!(meta.form, Some(School::Evocation));
    }

    #[test]
    fn meta_form_and_subschool_default_to_none() {
        // classic spells (the 542) and non-spell abilities leave both None
        let meta: AbilityMeta = ron::from_str("(school: Some(Evocation))")
            .expect("classic-school meta must deserialize");
        assert_eq!(meta.form, None);
        assert_eq!(meta.subschool, None);
    }

    // M4 (ENG-C1): Hemomancy's per-spell HP cost (the "blood price"); serde-default
    // so non-Hemomancy abilities omit it.
    #[test]
    fn meta_parses_hp_cost() {
        let meta: AbilityMeta =
            ron::from_str("(school: Some(Hemomancy), form: Some(Necromancy), hp_cost: Some(8.0))")
                .expect("AbilityMeta with hp_cost must deserialize");
        assert_eq!(meta.hp_cost, Some(8.0));
    }

    #[test]
    fn meta_hp_cost_defaults_none() {
        let meta: AbilityMeta =
            ron::from_str("(school: Some(Evocation))").expect("meta must deserialize");
        assert_eq!(meta.hp_cost, None);
    }
}

#[cfg(test)]
mod oracle_gate_tests {
    use super::*;

    // The 5 oracle-flavored spells (augury, divination, commune,
    // contact_other_plane, legend_lore) don't exist yet, so this synthesizes
    // an `AbilityRequirements { oracle: true, .. }` directly instead of
    // loading a real spell RON. Proves the gate itself works before any
    // content depends on it.

    #[test]
    fn oracle_ability_refused_when_not_live() {
        let req = AbilityRequirements {
            oracle: true,
            ..Default::default()
        };
        assert!(!req.requirements_met(None, None, false, 1));
    }

    #[test]
    fn oracle_ability_accepted_when_live() {
        let req = AbilityRequirements {
            oracle: true,
            ..Default::default()
        };
        assert!(req.requirements_met(None, None, true, 1));
    }

    #[test]
    fn non_oracle_ability_ignores_oracle_liveness() {
        let req = AbilityRequirements::default();
        assert!(req.requirements_met(None, None, false, 1));
        assert!(req.requirements_met(None, None, true, 1));
    }
}

#[cfg(test)]
mod min_level_gate_tests {
    use super::*;

    #[test]
    fn no_min_level_is_met_at_level_one() {
        let req = AbilityRequirements::default();
        assert!(req.requirements_met(None, None, false, 1));
    }

    #[test]
    fn min_level_refused_below_threshold() {
        let req = AbilityRequirements {
            min_level: Some(20),
            ..Default::default()
        };
        assert!(!req.requirements_met(None, None, false, 19));
    }

    #[test]
    fn min_level_accepted_at_and_above_threshold() {
        let req = AbilityRequirements {
            min_level: Some(20),
            ..Default::default()
        };
        assert!(req.requirements_met(None, None, false, 20));
        assert!(req.requirements_met(None, None, false, 21));
    }

    #[test]
    fn min_level_and_oracle_gates_are_independent() {
        let req = AbilityRequirements {
            oracle: true,
            min_level: Some(20),
            ..Default::default()
        };
        // Level requirement met, but ORACLE is down: still refused.
        assert!(!req.requirements_met(None, None, false, 20));
        // Both requirements met: accepted.
        assert!(req.requirements_met(None, None, true, 20));
        // ORACLE live, but level requirement not met: still refused.
        assert!(!req.requirements_met(None, None, true, 19));
    }
}

#[cfg(test)]
mod ground_aoe_tests {
    // JoinData is impractical to construct in isolation; behaviour is covered
    // by the deserialization test below and the in-game check. Here we pin
    // the RON contract.
    use crate::{assets::AssetExt, comp::CharacterAbility};

    #[test]
    fn shatterburst_deserializes_as_ground_aoe() {
        let ability = crate::assets::Ron::<CharacterAbility>::load_expect(
            "common.abilities.spells.ruin.shatterburst",
        )
        .read()
        .0
        .clone();
        assert!(matches!(ability, CharacterAbility::GroundAoe { .. }));
    }

    #[test]
    fn cc_spells_deserialize() {
        for id in [
            "common.abilities.spells.hollow.dread_whisper",
            "common.abilities.spells.gravesong.censure",
        ] {
            crate::assets::Ron::<CharacterAbility>::load_expect(id).read();
        }
    }

    #[test]
    fn cantrips_deserialize() {
        for id in [
            "common.abilities.spells.arcane.cinderbolt",
            "common.abilities.spells.divine.dawnmote",
            "common.abilities.spells.primordial.thornspit",
        ] {
            crate::assets::Ron::<CharacterAbility>::load_expect(id).read();
        }
    }
}

#[cfg(test)]
mod hold_spell_rebalance_tests {
    // hold_person/hold_monster/irresistible_dance grant Paralyzed at 100%
    // chance with no caster-side fail-roll gate and a flat 60-second
    // duration for all three, independent of each spell's own level. Pins
    // the fix: each of the 3 now carries the same CasterLevelRoll curve as
    // the other Paralyzed-granting spells, and each has its own duration
    // scaled to its own spell level (unlock_level == spell_level * 6, the
    // class-level-unlock table's `floor(class_level / 6)` formula inverted).
    use crate::{
        assets::AssetExt,
        combat::{CombatEffect, CombatRequirement},
        comp::{CharacterAbility, ClassKind},
    };

    struct Case {
        id: &'static str,
        // spell level (compendium.ron) * 6, per the spell-level-unlock table
        expected_unlock_level: u16,
        expected_dur_secs: f64,
    }

    #[test]
    fn hold_and_dance_spells_gain_caster_level_roll_and_level_scaled_duration() {
        let cases = [
            Case {
                id: "common.abilities.spells.arcane.hold_person",
                expected_unlock_level: 12, // spell level 2 * 6
                expected_dur_secs: 7.0,
            },
            Case {
                id: "common.abilities.spells.arcane.hold_monster",
                expected_unlock_level: 30, // spell level 5 * 6
                expected_dur_secs: 21.0,
            },
            Case {
                id: "common.abilities.spells.arcane.irresistible_dance",
                expected_unlock_level: 36, // spell level 6 * 6
                expected_dur_secs: 25.0,
            },
        ];

        for case in cases {
            let ability = crate::assets::Ron::<CharacterAbility>::load_expect(case.id)
                .read()
                .0
                .clone();
            let CharacterAbility::BasicRanged {
                projectile, meta, ..
            } = ability
            else {
                panic!(
                    "{} did not deserialize as BasicRanged: {ability:?}",
                    case.id
                );
            };
            let attack = projectile
                .attack
                .unwrap_or_else(|| panic!("{} has no projectile attack", case.id));
            let (effect, requirement) = attack.attack_effect.unwrap_or_else(|| {
                panic!(
                    "{} has no attack_effect -- still using an inline `buff:` with no \
                     CasterLevelRoll gate?",
                    case.id
                )
            });
            let CombatEffect::Buff(buff) = effect else {
                panic!("{} attack_effect is not a Buff: {effect:?}", case.id);
            };
            assert_eq!(
                buff.dur_secs.0, case.expected_dur_secs,
                "{} Paralyzed dur_secs",
                case.id
            );
            assert_eq!(buff.chance, 1.0, "{} buff chance", case.id);

            let CombatRequirement::CasterLevelRoll(curve) = requirement else {
                panic!(
                    "{} attack_effect requirement is not CasterLevelRoll: {requirement:?}",
                    case.id
                );
            };
            assert_eq!(
                curve.unlock_level, case.expected_unlock_level,
                "{} unlock_level",
                case.id
            );
            assert_eq!(
                curve.fail_chance_at_unlock, 0.25,
                "{} fail_chance_at_unlock",
                case.id
            );
            assert_eq!(
                curve.fail_chance_at_max_level, 0.05,
                "{} fail_chance_at_max_level",
                case.id
            );
            assert_eq!(
                curve.source_classes,
                vec![ClassKind::Mage],
                "{} source_classes",
                case.id
            );

            assert_eq!(
                meta.requirements.min_level,
                Some(case.expected_unlock_level),
                "{} ability-level min_level gate",
                case.id
            );
        }
    }
}

#[cfg(test)]
mod innate_tests {
    use crate::{
        assets::AssetExt,
        comp::{CharacterAbility, item::tool::AbilitySpec},
    };

    // The six racial innate RONs parse as their expected CharacterAbility variant
    // (magic-abilities plan Task 14).
    #[test]
    fn innate_ability_rons_load() {
        let cases: [(&str, fn(&CharacterAbility) -> bool); 6] = [
            ("human", |a| matches!(a, CharacterAbility::SelfBuff { .. })),
            ("elf", |a| matches!(a, CharacterAbility::SelfBuff { .. })),
            ("dwarf", |a| matches!(a, CharacterAbility::SelfBuff { .. })),
            ("orc", |a| matches!(a, CharacterAbility::SelfBuff { .. })),
            ("danari", |a| matches!(a, CharacterAbility::Blink { .. })),
            ("draugr", |a| {
                matches!(a, CharacterAbility::Shockwave { .. })
            }),
        ];
        for (species, is_expected) in cases {
            let id = format!("common.abilities.innate.{species}");
            let ability = crate::assets::Ron::<CharacterAbility>::load_expect(&id)
                .read()
                .0
                .clone();
            assert!(
                is_expected(&ability),
                "innate.{species} loaded as an unexpected variant: {ability:?}"
            );
        }
    }

    // Every humanoid species' innate set-key (from the grant logic itself) resolves
    // to a real manifest set. Ties AbilityPool::innate_set_key to the manifest, so
    // a typo'd key or a new species left unmapped is caught here, not in-game.
    #[test]
    fn innate_set_key_matches_manifest() {
        use crate::comp::{ability::AbilityPool, humanoid::ALL_SPECIES};
        let map = crate::comp::item::tool::AbilityMap::load().read();
        for species in ALL_SPECIES {
            let key = AbilityPool::innate_set_key(species);
            assert!(
                map.get_ability_set(&AbilitySpec::Custom(key.to_string()))
                    .is_some(),
                "species {species:?} maps to {key}, which has no manifest set"
            );
        }
    }
}

#[cfg(test)]
mod class_ability_pool_tests {
    use crate::{
        assets::AssetExt,
        comp::{Body, ClassKind, ability::AbilityPool, humanoid, item::tool::AbilitySpec},
    };

    /// Build a minimal Human body for deterministic testing.
    fn human_body() -> Body {
        Body::Humanoid(humanoid::Body {
            species: humanoid::Species::Human,
            body_type: humanoid::BodyType::Male,
            hair_style: 0,
            beard: 0,
            accessory: 0,
            hair_color: 0,
            skin: 0,
            eye_color: 0,
            eyes: 0,
            height_scale: 0,
        })
    }

    /// `for_character` emits class keys BEFORE the racial innate key, in
    /// spec order.
    #[test]
    fn warrior_pool_order_is_class_then_racial() {
        let body = human_body();
        let pool = AbilityPool::for_character(
            &body,
            &crate::comp::CharacterClass::single(ClassKind::Warrior),
        );
        // First two entries are the Warrior class keys (signature, capstone).
        assert_eq!(
            pool.abilities.first().map(String::as_str),
            Some("class.warrior.rally")
        );
        assert_eq!(
            pool.abilities.get(1).map(String::as_str),
            Some("class.warrior.onslaught"),
        );
        // Third entry is the racial innate key (Human in this case).
        assert_eq!(
            pool.abilities.get(2).map(String::as_str),
            Some("innate.human")
        );
        assert_eq!(pool.abilities.len(), 3);
    }

    /// A multiclass character's secondary keys go strictly at the end,
    /// after the racial innate — never between the primary's keys and the
    /// racial innate, so granting a second class to an existing character
    /// never shifts the racial innate's index.
    #[test]
    fn multiclass_pool_appends_secondary_keys_after_racial() {
        let body = human_body();
        let character_class = crate::comp::CharacterClass {
            primary: ClassKind::Warrior,
            secondary: Some(ClassKind::Mage),
            secondary_level: 20,
            future_levels_to_secondary: false,
        };
        let pool = AbilityPool::for_character(&body, &character_class);
        // The Warrior has no spells, so its keys, the racial innate, and the
        // Mage's keys are still the first five entries in that exact order;
        // the Mage's spells follow.
        assert_eq!(pool.abilities[..5], [
            "class.warrior.rally",
            "class.warrior.onslaught",
            "innate.human",
            "class.mage.arcanesurge",
            "class.mage.arcanemastery",
        ]);
        assert!(pool.abilities[5..].iter().all(|k| k.starts_with("spells.")));

        // A single-class Warrior's pool is an exact prefix of this one -- the
        // primary+racial portion never changes shape when a second class is
        // granted; only the tail grows.
        let single = AbilityPool::for_character(
            &body,
            &crate::comp::CharacterClass::single(ClassKind::Warrior),
        );
        assert_eq!(
            &pool.abilities[..single.abilities.len()],
            &single.abilities[..]
        );
    }

    /// A non-proof class (e.g. Adventurer) gets only the racial innate — no
    /// class keys — preserving the legacy/empty-tree behaviour.
    #[test]
    fn adventurer_pool_has_only_racial_innate() {
        let body = human_body();
        let pool = AbilityPool::for_character(
            &body,
            &crate::comp::CharacterClass::single(ClassKind::Adventurer),
        );
        assert_eq!(pool.abilities.len(), 1);
        assert_eq!(pool.abilities[0], "innate.human");
    }

    /// Every class ability key emitted by `for_character` resolves to a real
    /// manifest set — a typo in any RON path or key name is caught here, not
    /// in-game (mirrors the innate_set_key_matches_manifest test).
    #[test]
    fn class_ability_keys_match_manifest() {
        let map = crate::comp::item::tool::AbilityMap::load().read();
        let proof_classes = [
            ClassKind::Warrior,
            ClassKind::Mage,
            ClassKind::Cleric,
            ClassKind::Rogue,
        ];
        let body = human_body();
        for class in proof_classes {
            let pool =
                AbilityPool::for_character(&body, &crate::comp::CharacterClass::single(class));
            for key in &pool.abilities {
                // Skip the trailing racial innate (already covered by innate tests).
                if key.starts_with("innate.") {
                    continue;
                }
                assert!(
                    map.get_ability_set(&AbilitySpec::Custom(key.clone()))
                        .is_some(),
                    "class {class:?} key '{key}' has no manifest set",
                );
            }
        }
    }

    /// The four signature ability RONs deserialize without error.
    #[test]
    fn signature_ability_rons_load() {
        use crate::comp::CharacterAbility;
        let ids = [
            "common.abilities.class.warrior.rally",
            "common.abilities.class.mage.arcanesurge",
            "common.abilities.class.cleric.mendinglight",
            "common.abilities.class.rogue.ambush",
        ];
        for id in ids {
            crate::assets::Ron::<CharacterAbility>::load_expect(id).read();
        }
    }

    /// BL-06 P2b — the four capstone ability RONs deserialize without error.
    #[test]
    fn capstone_ability_rons_load() {
        use crate::comp::CharacterAbility;
        let ids = [
            "common.abilities.class.warrior.onslaught",
            "common.abilities.class.mage.arcanemastery",
            "common.abilities.class.cleric.radiantchannel",
            "common.abilities.class.rogue.vanish",
        ];
        for id in ids {
            crate::assets::Ron::<CharacterAbility>::load_expect(id).read();
        }
    }
}

#[cfg(test)]
mod spell_gate_tests {
    use super::{
        AbilityInput, AbilityPool, ActiveAbilities, AuxiliaryAbility, SpellGate, may_bind_ability,
    };
    use crate::comp::{
        Body, CharacterClass, ClassKind, SkillSet, humanoid, item::tool::AbilityMap,
        spell::SpellCompendium,
    };

    /// Build a minimal Human body for deterministic testing.
    fn human_body() -> Body {
        Body::Humanoid(humanoid::Body {
            species: humanoid::Species::Human,
            body_type: humanoid::BodyType::Male,
            hair_style: 0,
            beard: 0,
            accessory: 0,
            hair_color: 0,
            skin: 0,
            eye_color: 0,
            eyes: 0,
            height_scale: 0,
        })
    }

    /// A skill set whose derived character level is exactly `level`.
    fn skill_set_at_level(level: u16) -> SkillSet {
        let mut skill_set = SkillSet::default();
        skill_set.set_level(level);
        assert_eq!(skill_set.character_level(), level, "test setup");
        skill_set
    }

    /// An `ActiveAbilities` whose auxiliary slot 0 holds pool index `index`,
    /// under the empty-handed auxiliary key so no equipped weapon is needed.
    fn bound_to_pool_index(index: usize) -> ActiveAbilities {
        let mut sets = hashbrown::HashMap::new();
        sets.insert((None, None), vec![AuxiliaryAbility::Innate(index)]);
        ActiveAbilities::from_auxiliary(sets, None)
    }

    /// The first pool index holding a spell of exactly `spell_level`.
    fn spell_index_of_level(pool: &AbilityPool, spell_level: u8) -> usize {
        (0..pool.abilities.len())
            .find(|i| {
                pool.spell_gate(*i)
                    .is_some_and(|gate| gate.spell_level == spell_level)
            })
            .unwrap_or_else(|| panic!("the compendium has a Mage spell of level {spell_level}"))
    }

    #[test]
    fn pool_appends_spells_after_the_racial_innate() {
        let body = human_body();
        let pool = AbilityPool::for_character(&body, &CharacterClass::single(ClassKind::Mage));

        // Class signature + capstone first, then the racial innate, then spells.
        assert_eq!(pool.abilities[0], "class.mage.arcanesurge");
        assert_eq!(pool.abilities[1], "class.mage.arcanemastery");
        assert_eq!(pool.abilities[2], "innate.human");
        assert!(pool.abilities[3..].iter().all(|k| k.starts_with("spells.")));
        assert!(
            pool.abilities.len() > 3,
            "the Mage has spells in the compendium"
        );
        // The parallel invariant, everywhere.
        assert_eq!(pool.abilities.len(), pool.spell_gates.len());
        assert!(pool.spell_gates[..3].iter().all(Option::is_none));
        assert!(pool.spell_gates[3..].iter().all(Option::is_some));
    }

    /// The ordering contract: persisted hotbar slots store `Innate:index:N`
    /// positions, so granting a second class must APPEND only — every index
    /// that already existed must still name the same key afterwards.
    ///
    /// A gate may legitimately gain a grantor class in place (a spell both
    /// held classes list), which changes no index; that is asserted here as
    /// the only permitted difference.
    #[test]
    fn granting_a_second_class_shifts_no_existing_index() {
        let body = human_body();
        let before = AbilityPool::for_character(&body, &CharacterClass::single(ClassKind::Mage));
        let after = AbilityPool::for_character(&body, &mage_cleric());

        assert!(after.abilities.len() > before.abilities.len());
        assert_eq!(
            &after.abilities[..before.abilities.len()],
            &before.abilities[..],
            "every pre-existing index must still name the same key"
        );

        for (index, before_gate) in before.spell_gates.iter().enumerate() {
            let after_gate = &after.spell_gates[index];
            match (before_gate, after_gate) {
                (None, None) => {},
                (Some(before_gate), Some(after_gate)) => {
                    assert_eq!(
                        before_gate.spell_level, after_gate.spell_level,
                        "index {index} changed spell level"
                    );
                    // The Mage was and remains the first grantor; the Cleric
                    // may have been merged in beside it, never in front.
                    assert_eq!(
                        after_gate.classes().next(),
                        before_gate.classes().next(),
                        "index {index} changed its primary grantor"
                    );
                    for class in before_gate.classes() {
                        assert!(
                            after_gate.granted_by(class),
                            "index {index} lost grantor {class:?}"
                        );
                    }
                },
                _ => panic!("index {index} changed between spell and non-spell"),
            }
        }
    }

    /// A Mage(primary)/Cleric(secondary) character.
    fn mage_cleric() -> CharacterClass {
        let mut multi = CharacterClass::single(ClassKind::Mage);
        multi.secondary = Some(ClassKind::Cleric);
        multi
    }

    /// The key and level of a compendium spell BOTH `Mage` and `Cleric` can
    /// cast, above cantrip level so there is a band to be locked out of.
    /// Derived from the compendium rather than named, so re-authoring content
    /// cannot silently make this suite vacuous.
    fn shared_mage_cleric_spell() -> (String, u8) {
        let book = SpellCompendium::load_expect_cloned();
        let def = book
            .iter()
            .find(|s| {
                s.level > 0
                    && s.classes.contains(&ClassKind::Mage)
                    && s.classes.contains(&ClassKind::Cleric)
            })
            .expect("the compendium has a non-cantrip spell listed for both Cleric and Mage");
        (def.id.clone(), def.level)
    }

    fn gate_for<'a>(pool: &'a AbilityPool, key: &str) -> &'a SpellGate {
        let index = pool
            .abilities
            .iter()
            .position(|k| k == key)
            .unwrap_or_else(|| panic!("'{key}' is not in the pool"));
        pool.spell_gate(index)
            .unwrap_or_else(|| panic!("'{key}' carries no spell gate"))
    }

    #[test]
    fn a_spell_both_held_classes_grant_appears_once_and_records_both() {
        let body = human_body();
        let pool = AbilityPool::for_character(&body, &mage_cleric());
        let (key, _) = shared_mage_cleric_spell();

        // Emitted once, and the parallel-array invariant survives the merge.
        assert_eq!(
            pool.abilities.iter().filter(|k| *k == &key).count(),
            1,
            "'{key}' must be emitted exactly once"
        );
        let mut seen = std::collections::HashSet::new();
        for k in &pool.abilities {
            assert!(seen.insert(k.clone()), "duplicate pool key: {k}");
        }
        assert_eq!(pool.abilities.len(), pool.spell_gates.len());

        // ... but its gate names BOTH grantors, primary first.
        let classes: Vec<ClassKind> = gate_for(&pool, &key).classes().collect();
        assert_eq!(classes, vec![ClassKind::Mage, ClassKind::Cleric]);
    }

    #[test]
    fn a_shared_spell_unlocks_off_whichever_held_class_reached_the_band() {
        let body = human_body();
        let multi = mage_cleric();
        let pool = AbilityPool::for_character(&body, &multi);
        let (key, spell_level) = shared_mage_cleric_spell();
        let gate = gate_for(&pool, &key);
        assert_eq!(gate.spell_level, spell_level);

        // Mage 1 / Cleric 59: the Cleric side is far past the band. Before the
        // gate recorded both grantors this read the Mage's level 1 and locked
        // a spell the character's level-59 Cleric plainly knows.
        let mut mage_1_cleric_59 = multi;
        mage_1_cleric_59.set_secondary_level(59, 60);
        assert!(
            gate.is_unlocked(Some(&mage_1_cleric_59), 60),
            "the Cleric side must unlock it"
        );

        // Mage 59 / Cleric 1: the same spell off the other side.
        let mut mage_59_cleric_1 = multi;
        mage_59_cleric_1.set_secondary_level(1, 60);
        assert!(
            gate.is_unlocked(Some(&mage_59_cleric_1), 60),
            "the Mage side must unlock it"
        );

        // Mage 1 / Cleric 1: neither side has reached the band.
        let mut both_at_1 = multi;
        both_at_1.set_secondary_level(1, 2);
        assert!(
            !gate.is_unlocked(Some(&both_at_1), 2),
            "neither side has reached the band"
        );
    }

    /// The concrete case that exposed the single-grantor gate: a level-7
    /// `[Cleric, Mage]` spell on a Mage(1)/Cleric(59) character. Reading only
    /// the primary's level locked a spell the character's level-59 Cleric
    /// plainly knows.
    #[test]
    fn a_level_59_secondary_unlocks_its_own_high_level_shared_spell() {
        const KEY: &str = "spells.transmutation.regenerate";

        let book = SpellCompendium::load_expect_cloned();
        let def = book.get(KEY).expect("'{KEY}' is in the compendium");
        assert_eq!(def.level, 7);
        assert!(def.classes.contains(&ClassKind::Cleric) && def.classes.contains(&ClassKind::Mage));

        let pool = AbilityPool::for_character(&human_body(), &mage_cleric());
        let gate = gate_for(&pool, KEY);

        let mut mage_1_cleric_59 = mage_cleric();
        mage_1_cleric_59.set_secondary_level(59, 60);
        assert_eq!(
            gate.nearest_grantor(Some(&mage_1_cleric_59), 60),
            Some((ClassKind::Cleric, 59))
        );
        assert!(gate.is_unlocked(Some(&mage_1_cleric_59), 60));
    }

    #[test]
    fn a_single_class_character_gates_a_shared_spell_on_that_class_alone() {
        let body = human_body();
        let (key, _) = shared_mage_cleric_spell();

        for class in [ClassKind::Mage, ClassKind::Cleric] {
            let single = CharacterClass::single(class);
            let pool = AbilityPool::for_character(&body, &single);
            let gate = gate_for(&pool, &key);
            assert_eq!(
                gate.classes().collect::<Vec<_>>(),
                vec![class],
                "a single-class {class:?} records only its own class"
            );
            // The class the character does NOT hold can never unlock it.
            let other = if class == ClassKind::Mage {
                ClassKind::Cleric
            } else {
                ClassKind::Mage
            };
            assert!(
                !gate.is_unlocked(Some(&CharacterClass::single(other)), 60),
                "a {other:?} must not unlock a gate recorded for {class:?} only"
            );
        }
    }

    #[test]
    fn the_nearest_grantor_is_the_class_that_will_reach_the_band_first() {
        let body = human_body();
        let multi = mage_cleric();
        let pool = AbilityPool::for_character(&body, &multi);
        let (key, spell_level) = shared_mage_cleric_spell();
        let gate = gate_for(&pool, &key);

        // Cleric is further along, so it is the class the UI must name.
        let mut mage_20_cleric_40 = multi;
        mage_20_cleric_40.set_secondary_level(40, 60);
        assert_eq!(
            gate.nearest_grantor(Some(&mage_20_cleric_40), 60),
            Some((ClassKind::Cleric, 40))
        );

        // With the split reversed it names the Mage instead.
        let mut mage_40_cleric_20 = multi;
        mage_40_cleric_20.set_secondary_level(20, 60);
        assert_eq!(
            gate.nearest_grantor(Some(&mage_40_cleric_20), 60),
            Some((ClassKind::Mage, 40))
        );

        // A character holding neither grantor, or none at all, has no answer.
        assert!(
            gate.nearest_grantor(Some(&CharacterClass::single(ClassKind::Warrior)), 60)
                .is_none()
        );
        assert!(gate.nearest_grantor(None, 60).is_none());

        // The requirement the UI prints alongside it.
        assert_eq!(
            gate.required_class_level(),
            6 * u16::from(spell_level),
            "six class levels per spell level"
        );
    }

    #[test]
    fn a_cantrips_requirement_is_class_level_one_not_zero() {
        // Class levels start at 1, so a cantrip's requirement must not read 0.
        assert_eq!(SpellGate::new(ClassKind::Mage, 0).required_class_level(), 1);
    }

    #[test]
    fn add_class_is_idempotent_and_capped_at_two() {
        let mut gate = SpellGate::new(ClassKind::Mage, 3);
        gate.add_class(ClassKind::Mage);
        assert_eq!(gate.classes().collect::<Vec<_>>(), vec![ClassKind::Mage]);

        gate.add_class(ClassKind::Cleric);
        assert_eq!(gate.classes().collect::<Vec<_>>(), vec![
            ClassKind::Mage,
            ClassKind::Cleric
        ]);
        assert!(gate.granted_by(ClassKind::Cleric));
        assert!(!gate.granted_by(ClassKind::Warlock));

        // `CharacterClass` holds at most two classes, so a third can never
        // arrive from a real pool; if one ever did it must not corrupt the
        // gate.
        gate.add_class(ClassKind::Warlock);
        assert_eq!(gate.classes().collect::<Vec<_>>(), vec![
            ClassKind::Mage,
            ClassKind::Cleric
        ]);
    }

    #[test]
    fn cantrips_unlock_at_class_level_one_and_level_one_spells_at_six() {
        let cantrip = SpellGate::new(ClassKind::Mage, 0);
        let lvl1 = SpellGate::new(ClassKind::Mage, 1);
        let cc = CharacterClass::single(ClassKind::Mage);

        assert!(cantrip.is_unlocked(Some(&cc), 1));
        assert!(!lvl1.is_unlocked(Some(&cc), 5));
        assert!(lvl1.is_unlocked(Some(&cc), 6));
    }

    #[test]
    fn multiclass_gates_off_the_classs_own_level_not_the_characters() {
        // Warrior 40 / Warlock 20, character level 60.
        let mut cc = CharacterClass::single(ClassKind::Warrior);
        cc.secondary = Some(ClassKind::Warlock);
        cc.set_secondary_level(20, 60);

        let lvl3 = SpellGate::new(ClassKind::Warlock, 3);
        let lvl4 = SpellGate::new(ClassKind::Warlock, 4);
        // Warlock class level 20 -> floor(20/6) = 3.
        assert!(lvl3.is_unlocked(Some(&cc), 60));
        assert!(
            !lvl4.is_unlocked(Some(&cc), 60),
            "60 char levels must NOT grant level 4"
        );
    }

    #[test]
    fn a_gate_for_a_class_the_character_does_not_hold_fails_closed() {
        let cc = CharacterClass::single(ClassKind::Mage);
        let cleric_cantrip = SpellGate::new(ClassKind::Cleric, 0);
        assert!(!cleric_cantrip.is_unlocked(Some(&cc), 60));
        assert!(
            !cleric_cantrip.is_unlocked(None, 60),
            "no CharacterClass means refuse"
        );
    }

    #[test]
    fn is_unlocked_defaults_open_for_non_spell_and_out_of_range_indices() {
        let body = human_body();
        let pool = AbilityPool::for_character(&body, &CharacterClass::single(ClassKind::Warrior));
        assert!(
            pool.is_unlocked(0, None, 1),
            "class keys keep their old behaviour"
        );
        assert!(
            pool.is_unlocked(9_999, None, 1),
            "out of range must not panic"
        );
        assert!(pool.spell_gate(0).is_none());
        assert!(pool.spell_gate(9_999).is_none());
    }

    #[test]
    fn npc_pools_from_for_body_carry_no_spell_gates() {
        let body = human_body();
        let pool = AbilityPool::for_body(&body);
        assert_eq!(pool.abilities.len(), pool.spell_gates.len());
        assert!(pool.spell_gates.iter().all(Option::is_none));
    }

    /// A class with no authored spells keeps exactly the pool it had before
    /// spells entered it.
    #[test]
    fn a_spell_less_class_pool_is_unchanged() {
        let body = human_body();
        let pool = AbilityPool::for_character(&body, &CharacterClass::single(ClassKind::Warrior));
        assert_eq!(pool.abilities, vec![
            "class.warrior.rally",
            "class.warrior.onslaught",
            "innate.human",
        ]);
        assert_eq!(pool.spell_gates, vec![None, None, None]);
    }

    #[test]
    fn all_available_abilities_excludes_spells() {
        let body = human_body();
        let pool = AbilityPool::for_character(&body, &CharacterClass::single(ClassKind::Mage));
        let listed = ActiveAbilities::all_available_abilities(None, None, Some(&pool));
        let innate: Vec<usize> = listed
            .iter()
            .filter_map(|a| match a {
                AuxiliaryAbility::Innate(i) => Some(*i),
                _ => None,
            })
            .collect();
        assert_eq!(innate, vec![0, 1, 2], "only the class + racial keys");
    }

    #[test]
    fn all_available_spells_lists_every_spell_with_its_unlocked_flag() {
        let body = human_body();
        let cc = CharacterClass::single(ClassKind::Mage);
        let pool = AbilityPool::for_character(&body, &cc);
        let spell_count = pool.spell_gates.iter().filter(|g| g.is_some()).count();

        // At class level 1 every spell is listed, but only the cantrips are
        // castable.
        let at_1 = ActiveAbilities::all_available_spells(Some(&pool), Some(&cc), 1);
        assert_eq!(at_1.len(), spell_count);
        for (ability, unlocked) in &at_1 {
            let AuxiliaryAbility::Innate(i) = ability else {
                panic!("all_available_spells must only yield Innate entries")
            };
            let gate = pool.spell_gate(*i).expect("spell index");
            assert_eq!(*unlocked, gate.spell_level == 0);
        }
        assert!(at_1.iter().any(|(_, unlocked)| *unlocked), "cantrips");
        assert!(at_1.iter().any(|(_, unlocked)| !*unlocked), "higher levels");

        // At level 60 everything the compendium holds (levels 0-9) is open.
        let at_60 = ActiveAbilities::all_available_spells(Some(&pool), Some(&cc), 60);
        assert_eq!(at_60.len(), spell_count);
        assert!(at_60.iter().all(|(_, unlocked)| *unlocked));

        // No pool, no spells.
        assert!(ActiveAbilities::all_available_spells(None, Some(&cc), 60).is_empty());
    }

    /// Bind pool `index` to auxiliary slot 0 and try to activate it at
    /// `character_level`; `true` when the activation produced an ability.
    fn activates(
        body: &Body,
        pool: &AbilityPool,
        character_class: Option<&CharacterClass>,
        index: usize,
        character_level: u16,
        ability_map: &AbilityMap,
    ) -> bool {
        bound_to_pool_index(index)
            .activate_ability(
                AbilityInput::Auxiliary(0),
                None,
                None,
                &skill_set_at_level(character_level),
                Some(body),
                None,
                None,
                None,
                None,
                None,
                Some(pool),
                character_class,
                ability_map,
            )
            .is_some()
    }

    #[test]
    fn activate_ability_refuses_a_locked_spell_and_allows_an_unlocked_one() {
        let body = human_body();
        let cc = CharacterClass::single(ClassKind::Mage);
        let pool = AbilityPool::for_character(&body, &cc);
        let ability_map = AbilityMap::load();
        let ability_map = ability_map.read();
        let index = spell_index_of_level(&pool, 1);

        assert!(
            !activates(&body, &pool, Some(&cc), index, 1, &ability_map),
            "a class-level-1 Mage must not cast a level-1 spell"
        );
        assert!(
            activates(&body, &pool, Some(&cc), index, 6, &ability_map),
            "the same character at class level 6 must cast it"
        );
    }

    #[test]
    fn a_cantrip_activates_at_class_level_one() {
        let body = human_body();
        let cc = CharacterClass::single(ClassKind::Mage);
        let pool = AbilityPool::for_character(&body, &cc);
        let ability_map = AbilityMap::load();
        let ability_map = ability_map.read();
        let index = spell_index_of_level(&pool, 0);

        assert!(activates(&body, &pool, Some(&cc), index, 1, &ability_map));
    }

    #[test]
    fn a_spell_of_a_class_the_character_does_not_hold_never_activates() {
        // A Mage carrying a Cleric-gated key (only reachable by hand-crafting
        // the pool) must be refused at every level: the gate fails closed.
        let body = human_body();
        let cc = CharacterClass::single(ClassKind::Mage);
        let mut pool = AbilityPool::for_character(&body, &cc);
        let index = spell_index_of_level(&pool, 0);
        pool.spell_gates[index] = Some(SpellGate::new(ClassKind::Cleric, 0));
        let ability_map = AbilityMap::load();
        let ability_map = ability_map.read();

        assert!(!activates(&body, &pool, Some(&cc), index, 60, &ability_map));
    }

    #[test]
    fn npc_innate_activation_is_unaffected_by_the_gate() {
        // An NPC pool (`for_body`) carries no gates and no `CharacterClass`,
        // so its racial innate activates exactly as it did before the gate
        // existed.
        let body = human_body();
        let pool = AbilityPool::for_body(&body);
        let ability_map = AbilityMap::load();
        let ability_map = ability_map.read();
        assert_eq!(pool.abilities, vec!["innate.human"]);

        assert!(
            activates(&body, &pool, None, 0, 1, &ability_map),
            "a gate-free pool entry stays castable with no CharacterClass"
        );
    }

    #[test]
    fn may_bind_rejects_a_locked_spell_and_accepts_an_unlocked_one() {
        let body = human_body();
        let cc = CharacterClass::single(ClassKind::Mage);
        let pool = AbilityPool::for_character(&body, &cc);
        let locked = spell_index_of_level(&pool, 1);
        let cantrip = spell_index_of_level(&pool, 0);

        assert!(!may_bind_ability(
            Some(&pool),
            Some(&cc),
            1,
            AuxiliaryAbility::Innate(locked)
        ));
        assert!(may_bind_ability(
            Some(&pool),
            Some(&cc),
            6,
            AuxiliaryAbility::Innate(locked)
        ));
        assert!(may_bind_ability(
            Some(&pool),
            Some(&cc),
            1,
            AuxiliaryAbility::Innate(cantrip)
        ));
    }

    #[test]
    fn may_bind_accepts_non_spell_abilities_unconditionally() {
        let body = human_body();
        let cc = CharacterClass::single(ClassKind::Mage);
        let pool = AbilityPool::for_character(&body, &cc);

        // A class-signature key: an `Innate` index carrying no gate.
        assert!(may_bind_ability(
            Some(&pool),
            Some(&cc),
            1,
            AuxiliaryAbility::Innate(0)
        ));
        // Weapon / glider / empty bindings are outside this predicate's remit
        // and keep their (unvalidated) behaviour, pool or no pool.
        assert!(may_bind_ability(
            Some(&pool),
            Some(&cc),
            1,
            AuxiliaryAbility::MainWeapon(99)
        ));
        assert!(may_bind_ability(None, None, 1, AuxiliaryAbility::Empty));
        assert!(may_bind_ability(None, None, 1, AuxiliaryAbility::Glider(0)));
    }

    #[test]
    fn may_bind_refuses_an_innate_binding_when_the_entity_has_no_pool() {
        // An entity with no `AbilityPool` has no innate abilities to bind, so
        // every `Innate(_)` write from such a client is dropped.
        let cc = CharacterClass::single(ClassKind::Mage);
        assert!(!may_bind_ability(
            None,
            Some(&cc),
            60,
            AuxiliaryAbility::Innate(0)
        ));
        assert!(!may_bind_ability(
            None,
            None,
            60,
            AuxiliaryAbility::Innate(0)
        ));
    }

    #[test]
    fn may_bind_refuses_an_out_of_range_innate_index_only_when_gated() {
        // Out-of-range indices answer `true` (they resolve to nothing at use
        // time, exactly as before this change) — the predicate narrows the
        // hole for spells, it does not become a general bounds check.
        let body = human_body();
        let cc = CharacterClass::single(ClassKind::Mage);
        let pool = AbilityPool::for_character(&body, &cc);
        assert!(may_bind_ability(
            Some(&pool),
            Some(&cc),
            1,
            AuxiliaryAbility::Innate(9_999)
        ));
    }
}
