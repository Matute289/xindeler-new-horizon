pub mod ability;
mod admin;
pub mod agent;
pub mod anchor;
pub mod arcing;
pub mod attunement;
pub mod aura;
pub mod background;
pub mod banished;
pub mod beam;
pub mod body;
pub mod buff;
pub mod character_state;
pub mod chat;
pub mod class;
pub mod combo;
pub mod compass;
pub mod controller;
pub mod creature_type;
pub mod detection;
mod energy;
pub mod ethos;
pub mod fluid_dynamics;
pub mod gizmos;
pub mod group;
mod hardcore;
mod health;
mod inputs;
pub mod inventory;
pub mod invite;
pub mod item_condition;
mod last;
mod location;
pub mod loot_owner;
pub mod melee;
pub mod misc;
pub mod ori;
pub mod pet;
mod phys;
mod player;
pub mod poise;
pub mod pool;
pub mod presence;
pub mod projectile;
pub mod remote_sense;
pub mod shockwave;
pub mod skillset;
pub mod spell;
pub mod spell_mastery;
mod stats;
pub mod teleport;
pub mod transcription;
pub mod trigger;
pub mod visual;

// Reexports
pub use self::{
    ability::{
        Ability, AbilityCooldowns, AbilityInput, AbilityPool, ActiveAbilities, BASE_ABILITY_LIMIT,
        CharacterAbility, CharacterAbilityType, MagicSource, School, Stance,
    },
    admin::{Admin, AdminRole},
    agent::{
        Agent, Alignment, Behavior, BehaviorCapability, BehaviorState, PidController,
        TradingBehavior,
    },
    anchor::Anchor,
    arcing::{ArcProperties, Arcing},
    attunement::{
        AttunedItems, Attuning, attune_time, item_effects_active, max_attuned_items,
        reconcile_attunement,
    },
    aura::{Aura, AuraChange, AuraKind, Auras, EnteredAuras},
    background::{Background, BackgroundKind},
    banished::{Banished, BanishmentId},
    beam::Beam,
    body::{
        AllBodies, Body, BodyData, Gender, arthropod, biped_large, biped_small, bird_large,
        bird_medium, crustacean, dragon, fish_medium, fish_small, golem, humanoid, object, plugin,
        quadruped_low, quadruped_medium, quadruped_small, ship, theropod,
    },
    buff::{
        Buff, BuffCategory, BuffChange, BuffData, BuffEffect, BuffKey, BuffKind, BuffSource, Buffs,
        ModifierKind,
    },
    character_state::{CharacterActivity, CharacterState, StateUpdate},
    chat::{
        ChatMode, ChatMsg, ChatType, Faction, SpeechBubble, SpeechBubbleType, UnresolvedChatMsg,
    },
    class::{CharacterClass, ClassKind, EquipmentGroup},
    combo::Combo,
    controller::{
        ControlAction, ControlEvent, Controller, ControllerInputs, GroupManip, InputAttr,
        InputKind, InventoryAction, InventoryEvent, InventoryManip, UtteranceKind,
    },
    creature_type::{
        AberrationSubtype, BeastSubtype, CelestialSubtype, ConstructSubtype, CreatureKind,
        CreatureSubtype, CreatureTags, DragonSubtype, ElementalSubtype, FeySubtype, FiendSubtype,
        GiantSubtype, HumanoidSubtype, MonstrositySubtype, OozeSubtype, PlantSubtype,
        UndeadSubtype,
    },
    detection::{
        ConcealedUnlessTrueSight, DetectDetail, Detected, DetectedEntity, DetectedPoint, SenseKind,
        observer_pierces_concealment,
    },
    energy::Energy,
    ethos::{Ethos, Moral, Order},
    fluid_dynamics::Fluid,
    gizmos::GizmoSubscriber,
    group::Group,
    hardcore::Hardcore,
    inputs::CanBuild,
    inventory::{
        CollectFailedReason, Inventory, InventoryUpdateBuffer, InventoryUpdateEvent,
        item::{
            self, FrontendItem, Item, ItemConfig, ItemDrops, PickupItem, ThrownItem,
            item_key::ItemKey,
            tool::{self, AbilityItem},
        },
        recipe_book::RecipeBook,
        slot,
        spell_book::SpellBook,
    },
    item_condition::{ConditionPredicate, ItemCondition, item_condition_buff_data},
    last::Last,
    location::{MapMarker, MapMarkerChange, MapMarkerUpdate, Waypoint, WaypointArea},
    loot_owner::LootOwner,
    melee::{Melee, MeleeConstructor, MeleeConstructorKind},
    misc::Object,
    ori::Ori,
    pet::Pet,
    phys::{
        CapsulePrism, Collider, Density, ForceUpdate, Immovable, Mass, PhysicsState, Pos,
        PosVelOriDefer, PreviousPhysCache, Scale, Sticky, Vel,
    },
    player::{AliasError, DisconnectReason, MAX_ALIAS_LEN, Player},
    poise::{Poise, PoiseChange, PoiseState},
    pool::{Pool, PoolProperties},
    presence::{Presence, PresenceKind, SpectatingEntity},
    projectile::{Projectile, ProjectileConstructor},
    remote_sense::{RemoteSense, SenseAnchor},
    shockwave::{Shockwave, ShockwaveHitEntities},
    skillset::{
        SkillGroup, SkillGroupKind, SkillSet,
        skills::{self, Skill},
    },
    spell::{CLASS_LEVELS_PER_SPELL_LEVEL, SpellCompendium, SpellDef, spell_level_unlocked},
    spell_mastery::{
        MASTERY_XP_FULL, NON_DAMAGE_WEIGHT, POLYGLOT_BONUS_PER_RANK, SpellMastery,
        grant_source_mastery, level_delta_weight, mastery_tier_max_level,
    },
    stats::{ActiveSense, Stats, StatsModifier},
    teleport::Teleporting,
    trigger::{
        MAX_TRIGGER_SLOTS, SlotState, TRIGGER_SLOT_LEVELS, TriggerAbility, TriggerCondition,
        TriggerSlot, TriggerSlots, unlocked_trigger_slots,
    },
    visual::{FrontendMarker, LightAnimation, LightEmitter},
};
pub use common_i18n::{Content, LocalizationArg};

pub use health::{Health, HealthChange, is_downed, is_downed_or_dead};
