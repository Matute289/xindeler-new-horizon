use crate::{
    comp::{
        self, AbilityCooldowns, AbilityPool, ActiveAbilities, Alignment, AttunedItems, Beam, Body,
        Buffs, CharacterActivity, CharacterClass, CharacterState, Combo, ControlAction, Controller,
        ControllerInputs, Density, Energy, Health, Immovable, InputAttr, InputKind, Inventory,
        InventoryAction, Mass, Melee, Ori, PhysicsState, PickupItem, Pos, PreviousPhysCache, Scale,
        SkillSet, Stance, StateUpdate, Stats, Vel,
        body::parts::Heads,
        character_state::OutputEvents,
        item::{MaterialStatManifest, tool::AbilityMap},
    },
    link::Is,
    mounting::{Rider, VolumeRider},
    resources::{DeltaTime, OracleLive, Time},
    terrain::TerrainGrid,
    tether::Follower,
    uid::{IdMaps, Uid},
};
use specs::{Entity, LazyUpdate, Read, ReadStorage, storage::FlaggedAccessMut};
use vek::*;

pub trait CharacterBehavior {
    fn behavior(&self, data: &JoinData, output_events: &mut OutputEvents) -> StateUpdate;
    // Impl these to provide behavior for these inputs
    fn swap_equipped_weapons(
        &self,
        data: &JoinData,
        _output_events: &mut OutputEvents,
    ) -> StateUpdate {
        StateUpdate::from(data)
    }
    fn manipulate_loadout(
        &self,
        data: &JoinData,
        _output_events: &mut OutputEvents,
        _inv_action: InventoryAction,
    ) -> StateUpdate {
        StateUpdate::from(data)
    }
    fn wield(&self, data: &JoinData, _output_events: &mut OutputEvents) -> StateUpdate {
        StateUpdate::from(data)
    }
    fn glide_wield(&self, data: &JoinData, _output_events: &mut OutputEvents) -> StateUpdate {
        StateUpdate::from(data)
    }
    fn unwield(&self, data: &JoinData, _output_events: &mut OutputEvents) -> StateUpdate {
        StateUpdate::from(data)
    }
    fn sit(&self, data: &JoinData, _output_events: &mut OutputEvents) -> StateUpdate {
        StateUpdate::from(data)
    }
    fn crawl(&self, data: &JoinData, _output_events: &mut OutputEvents) -> StateUpdate {
        StateUpdate::from(data)
    }
    fn dance(&self, data: &JoinData, _output_events: &mut OutputEvents) -> StateUpdate {
        StateUpdate::from(data)
    }
    fn sneak(&self, data: &JoinData, _output_events: &mut OutputEvents) -> StateUpdate {
        StateUpdate::from(data)
    }
    fn stand(&self, data: &JoinData, _output_events: &mut OutputEvents) -> StateUpdate {
        StateUpdate::from(data)
    }
    fn talk(
        &self,
        data: &JoinData,
        _output_events: &mut OutputEvents,
        _tgt: Option<Uid>,
    ) -> StateUpdate {
        StateUpdate::from(data)
    }
    fn on_input(
        &self,
        data: &JoinData,
        _input: InputKind,
        _output_events: &mut OutputEvents,
    ) -> StateUpdate {
        StateUpdate::from(data)
    }

    fn start_input(
        &self,
        data: &JoinData,
        input: InputKind,
        target_entity: Option<Uid>,
        select_pos: Option<Vec3<f32>>,
        output_events: &mut OutputEvents,
    ) -> StateUpdate {
        let mut update = self.on_input(data, input, output_events);
        update.queued_inputs.insert(input, InputAttr {
            select_pos,
            target_entity,
        });
        update
    }
    fn cancel_input(&self, data: &JoinData, input: InputKind) -> StateUpdate {
        let mut update = StateUpdate::from(data);
        update.removed_inputs.push(input);
        update
    }
    fn handle_event(
        &self,
        data: &JoinData,
        output_events: &mut OutputEvents,
        event: ControlAction,
    ) -> StateUpdate {
        match event {
            ControlAction::SwapEquippedWeapons => self.swap_equipped_weapons(data, output_events),
            ControlAction::InventoryAction(inv_action) => {
                self.manipulate_loadout(data, output_events, inv_action)
            },
            ControlAction::Wield => self.wield(data, output_events),
            ControlAction::GlideWield => self.glide_wield(data, output_events),
            ControlAction::Unwield => self.unwield(data, output_events),
            ControlAction::Sit => self.sit(data, output_events),
            ControlAction::Crawl => self.crawl(data, output_events),
            ControlAction::Dance => self.dance(data, output_events),
            ControlAction::Sneak => {
                if data.mount_data.is_none() && data.volume_mount_data.is_none() {
                    self.sneak(data, output_events)
                } else {
                    self.stand(data, output_events)
                }
            },
            ControlAction::Stand => self.stand(data, output_events),
            ControlAction::Talk(tgt) => self.talk(data, output_events, tgt),
            ControlAction::StartInput {
                input,
                target_entity,
                select_pos,
            } => self.start_input(data, input, target_entity, select_pos, output_events),
            ControlAction::CancelInput { input } => self.cancel_input(data, input),
        }
    }
}

/// Read-Only Data sent from Character Behavior System to behavior fn's
pub struct JoinData<'a> {
    pub entity: Entity,
    pub uid: &'a Uid,
    pub character: &'a CharacterState,
    pub character_activity: &'a CharacterActivity,
    pub pos: &'a Pos,
    pub vel: &'a Vel,
    pub ori: &'a Ori,
    pub scale: Option<&'a Scale>,
    pub mass: &'a Mass,
    pub density: &'a Density,
    pub dt: &'a DeltaTime,
    pub time: &'a Time,
    /// Whether PROJECT ORACLE is live, read from the server's own resource so
    /// `requirements_paid` can gate `AbilityRequirements.oracle` abilities
    /// with server authority (see `OracleLive`).
    pub oracle_live: &'a OracleLive,
    pub controller: &'a Controller,
    pub inputs: &'a ControllerInputs,
    pub health: Option<&'a Health>,
    /// Whether the entity is a hardcore (permadeath) character — Hemomancy's
    /// HP-cost may be lethal for these (no 1-HP floor); see
    /// `hp_cost_affordable`.
    pub hardcore: bool,
    pub heads: Option<&'a Heads>,
    pub energy: &'a Energy,
    pub inventory: Option<&'a Inventory>,
    /// The wearer's attuned-item set, so unattuned gear grants no abilities
    /// (ENG-D2c). `None` is treated as "nothing attuned".
    pub attuned: Option<&'a AttunedItems>,
    pub body: &'a Body,
    pub physics: &'a PhysicsState,
    pub melee_attack: Option<&'a Melee>,
    pub updater: &'a LazyUpdate,
    pub stats: &'a Stats,
    pub skill_set: &'a SkillSet,
    pub active_abilities: Option<&'a ActiveAbilities>,
    pub ability_cooldowns: Option<&'a AbilityCooldowns>,
    pub ability_pool: Option<&'a AbilityPool>,
    /// Xindeler: the character's class(es), so the spell-level gate can read
    /// each held class's OWN level (`AbilityPool::is_unlocked`). `None` for
    /// NPCs, which have no `CharacterClass` and no spell keys in their pool.
    pub character_class: Option<&'a CharacterClass>,
    pub ability_map: &'a AbilityMap,
    pub msm: &'a MaterialStatManifest,
    pub combo: Option<&'a Combo>,
    pub alignment: Option<&'a comp::Alignment>,
    pub terrain: &'a TerrainGrid,
    pub mount_data: Option<&'a Is<Rider>>,
    pub volume_mount_data: Option<&'a Is<VolumeRider>>,
    pub stance: Option<&'a Stance>,
    pub id_maps: &'a Read<'a, IdMaps>,
    pub alignments: &'a ReadStorage<'a, Alignment>,
    pub prev_phys_caches: &'a ReadStorage<'a, PreviousPhysCache>,
    pub bodies: &'a ReadStorage<'a, Body>,
    pub buffs: Option<&'a Buffs>,
    /// Global lookup used by `TelekineticGrip` (and any future ranged-grab
    /// ability) to validate an arbitrary `target_entity` carries loose,
    /// grabbable loot — see `common/src/states/telekinetic_grip.rs`.
    pub pickup_items: &'a ReadStorage<'a, PickupItem>,
    /// Global lookup of the `Immovable` marker, so a grab ability can reject
    /// world-anchored targets.
    pub immovables: &'a ReadStorage<'a, Immovable>,
    /// Global lookup of `Is<Follower>` so a grab ability can tell whether an
    /// arbitrary target is already tethered to someone else.
    pub is_followers: &'a ReadStorage<'a, Is<Follower>>,
}

pub struct JoinStruct<'a> {
    pub entity: Entity,
    pub uid: &'a Uid,
    pub char_state: FlaggedAccessMut<'a, &'a mut CharacterState, CharacterState>,
    pub character_activity: FlaggedAccessMut<'a, &'a mut CharacterActivity, CharacterActivity>,
    pub pos: &'a mut Pos,
    pub vel: &'a mut Vel,
    pub ori: &'a mut Ori,
    pub scale: Option<&'a Scale>,
    pub mass: &'a Mass,
    pub density: FlaggedAccessMut<'a, &'a mut Density, Density>,
    pub energy: FlaggedAccessMut<'a, &'a mut Energy, Energy>,
    pub inventory: Option<&'a Inventory>,
    pub attuned: Option<&'a AttunedItems>,
    pub controller: &'a mut Controller,
    pub health: Option<&'a Health>,
    pub hardcore: bool,
    pub heads: Option<&'a Heads>,
    pub body: &'a Body,
    pub physics: &'a PhysicsState,
    pub melee_attack: Option<&'a Melee>,
    pub beam: Option<&'a Beam>,
    pub stat: &'a Stats,
    pub skill_set: &'a SkillSet,
    pub active_abilities: Option<&'a ActiveAbilities>,
    pub ability_cooldowns: Option<&'a AbilityCooldowns>,
    pub ability_pool: Option<&'a AbilityPool>,
    /// Xindeler: see [`JoinData::character_class`].
    pub character_class: Option<&'a CharacterClass>,
    pub combo: Option<&'a Combo>,
    pub alignment: Option<&'a comp::Alignment>,
    pub terrain: &'a TerrainGrid,
    pub mount_data: Option<&'a Is<Rider>>,
    pub volume_mount_data: Option<&'a Is<VolumeRider>>,
    pub stance: Option<&'a Stance>,
    pub id_maps: &'a Read<'a, IdMaps>,
    pub alignments: &'a ReadStorage<'a, Alignment>,
    pub prev_phys_caches: &'a ReadStorage<'a, PreviousPhysCache>,
    pub bodies: &'a ReadStorage<'a, Body>,
    pub buffs: Option<&'a Buffs>,
    pub pickup_items: &'a ReadStorage<'a, PickupItem>,
    pub immovables: &'a ReadStorage<'a, Immovable>,
    pub is_followers: &'a ReadStorage<'a, Is<Follower>>,
}

impl<'a> JoinData<'a> {
    pub fn new(
        j: &'a JoinStruct<'a>,
        updater: &'a LazyUpdate,
        dt: &'a DeltaTime,
        time: &'a Time,
        msm: &'a MaterialStatManifest,
        ability_map: &'a AbilityMap,
        oracle_live: &'a OracleLive,
    ) -> Self {
        Self {
            entity: j.entity,
            uid: j.uid,
            character: &j.char_state,
            character_activity: &j.character_activity,
            pos: j.pos,
            vel: j.vel,
            ori: j.ori,
            scale: j.scale,
            mass: j.mass,
            density: &j.density,
            energy: &j.energy,
            inventory: j.inventory,
            attuned: j.attuned,
            controller: j.controller,
            inputs: &j.controller.inputs,
            health: j.health,
            hardcore: j.hardcore,
            heads: j.heads,
            body: j.body,
            physics: j.physics,
            melee_attack: j.melee_attack,
            stats: j.stat,
            skill_set: j.skill_set,
            updater,
            dt,
            time,
            msm,
            ability_map,
            oracle_live,
            combo: j.combo,
            alignment: j.alignment,
            terrain: j.terrain,
            active_abilities: j.active_abilities,
            ability_cooldowns: j.ability_cooldowns,
            ability_pool: j.ability_pool,
            character_class: j.character_class,
            mount_data: j.mount_data,
            volume_mount_data: j.volume_mount_data,
            stance: j.stance,
            id_maps: j.id_maps,
            alignments: j.alignments,
            prev_phys_caches: j.prev_phys_caches,
            bodies: j.bodies,
            buffs: j.buffs,
            pickup_items: j.pickup_items,
            immovables: j.immovables,
            is_followers: j.is_followers,
        }
    }
}
