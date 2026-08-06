//! Contains an "x macro" for all synced components as well as [NetSync]
//! implementations for those components.
//!
//!
//! An x macro accepts another macro as input and calls it with a list of
//! inputs. This allows adding components to the list in the x macro declaration
//! and then writing macros that will accept this list and generate code that
//! handles every synced component without further repitition of the component
//! set.
//!
//! This module also re-exports all the component types that are synced.
//!
//! A glob import from this can be used so that the component types are in scope
//! when using the x macro defined here which requires this.

/// This provides a lowercase name and the component type.
///
/// See [module](self) level docs for more details.
#[macro_export]
macro_rules! synced_components {
    ($macro:ident) => {
        $macro! {
            body: Body,
            hardcore: Hardcore,
            character_class: CharacterClass,
            ethos: Ethos,
            background: Background,
            stats: Stats,
            buffs: Buffs,
            auras: Auras,
            energy: Energy,
            health: Health,
            heads: Heads,
            poise: Poise,
            light_emitter: LightEmitter,
            loot_owner: LootOwner,
            item: PickupItem,
            thrown_item: ThrownItem,
            scale: Scale,
            group: Group,
            is_mount: IsMount,
            is_rider: IsRider,
            is_volume_rider: IsVolumeRider,
            volume_riders: VolumeRiders,
            is_leader: IsLeader,
            is_follower: IsFollower,
            mass: Mass,
            density: Density,
            collider: Collider,
            sticky: Sticky,
            immovable: Immovable,
            character_state: CharacterState,
            character_activity: CharacterActivity,
            shockwave: Shockwave,
            beam: Beam,
            alignment: Alignment,
            stance: Stance,
            object: Object,
            frontend_marker: FrontendMarker,
            arcing: Arcing,
            // A property of the entity itself (like `Body`/`Pos`), not a
            // secret about who is observing it — see its own doc comment.
            concealed_unless_true_sight: ConcealedUnlessTrueSight,
            // A cast disguise's cosmetic lie about which `Body`/name an
            // entity appears to have. Visible to everyone nearby on purpose
            // — a disguise being *seen* (as a disguise) is the entire point
            // of casting one, unlike the owner-private components below.
            disguise: Disguise,
            // TODO: change this to `SyncFrom::ClientEntity` and sync the bare minimum
            // from other entities (e.g. just keys needed to show appearance
            // based on their loadout). Also, it looks like this actually has
            // an alternative sync method implemented in entity_sync via
            // ServerGeneral::InventoryUpdate so we could use that instead
            // or remove the part where it clones the inventory.
            inventory: Inventory,
            // TODO: this is used in combat rating calculation in voxygen but we can probably
            // remove it from that and then see if it's used for anything else and try to move
            // to only being synced like `ActiveAbilities`.
            skill_set: SkillSet,

            // Synced to the client only for its own entity

            admin: Admin,
            combo: Combo,
            active_abilities: ActiveAbilities,
            ability_cooldowns: AbilityCooldowns,
            ability_pool: AbilityPool,
            // Reactive trigger slots. Owner-private: nobody else's business
            // which spell a player has armed, and the wall-clock half of a
            // cooling slot never crosses the wire at all (it is `serde(skip)`);
            // the client only ever sees the in-game `Time` projection the
            // server computed for it.
            trigger_slots: TriggerSlots,
            // Per-`MagicSource` mastery progress. Owner-private, same
            // reasoning as `trigger_slots`: nobody else's business how far
            // along a Mage is on any source.
            spell_mastery: SpellMastery,
            // The attuned-item set (ENG-D2). `Attuning` (in-progress channels) is
            // NOT synced — it carries absolute server `Time`, a different epoch
            // than the client's, so a finish timestamp would be meaningless there.
            attuned_items: AttunedItems,
            // The active remote-sensing link (a spell-granted detached
            // viewpoint). Owner-private on purpose: broadcasting *who* is
            // currently watching through *what* would be a PvP information
            // leak, exactly like `detected` below. NEVER `AnyEntity`.
            remote_sense: RemoteSense,
            // The detection reveal set. Owner-private on purpose: a
            // concealment-piercing reveal broadcast to every nearby client
            // (like `Stats`) would leak the concealed entity's position to
            // everyone in range, not just the caster.
            detected: Detected,
            can_build: CanBuild,
            is_interactor: IsInteractor,
            interactors: Interactors,
        }
    };
}

macro_rules! reexport_comps {
    ($($name:ident: $type:ident,)*) => {
        mod inner {
            pub use common::comp::*;
            pub use body::parts::Heads;
            pub use common::{interaction::Interactors, mounting::VolumeRiders};
            use common::link::Is;
            use common::{
                mounting::{Mount, Rider, VolumeRider},
                tether::{Leader, Follower},
                interaction::{Interactor},
            };

            // We alias these because the identifier used for the
            // component's type is reused as an enum variant name
            // in the macro's that we pass to `synced_components!`.
            //
            // This is also the reason we need this inner module, since
            // we can't just re-export all the types directly from `common::comp`.
            pub type IsMount = Is<Mount>;
            pub type IsRider = Is<Rider>;
            pub type IsVolumeRider = Is<VolumeRider>;
            pub type IsLeader = Is<Leader>;
            pub type IsFollower = Is<Follower>;
            pub type IsInteractor = Is<Interactor>;
        }

        // Re-export all the component types. So that uses of `synced_components!` outside this
        // module can bring them into scope with a single glob import.
        $(pub use inner::$type;)*
    }
}
// Pass `reexport_comps` macro to the "x macro" which will invoke it with a list
// of components.
//
// Note: this brings all these components into scope for the implementations
// below.
synced_components!(reexport_comps);

// ===============================
// === NetSync implementations ===
// ===============================

use crate::sync::{NetSync, SyncFrom};

impl NetSync for Body {
    const SYNC_FROM: SyncFrom = SyncFrom::AnyEntity;
}

impl NetSync for Hardcore {
    const SYNC_FROM: SyncFrom = SyncFrom::AnyEntity;
}

impl NetSync for CharacterClass {
    const SYNC_FROM: SyncFrom = SyncFrom::AnyEntity;
}

impl NetSync for Ethos {
    const SYNC_FROM: SyncFrom = SyncFrom::AnyEntity;
}

impl NetSync for Background {
    // Mirrors CharacterClass/Ethos: visible on other players' character
    // sheets, not just the client's own entity.
    const SYNC_FROM: SyncFrom = SyncFrom::AnyEntity;
}

impl NetSync for Stats {
    const SYNC_FROM: SyncFrom = SyncFrom::AnyEntity;
}

impl NetSync for Buffs {
    const SYNC_FROM: SyncFrom = SyncFrom::AnyEntity;
}

impl NetSync for Auras {
    const SYNC_FROM: SyncFrom = SyncFrom::AnyEntity;
}

impl NetSync for Energy {
    const SYNC_FROM: SyncFrom = SyncFrom::AnyEntity;
}

impl NetSync for Health {
    const SYNC_FROM: SyncFrom = SyncFrom::AnyEntity;

    fn pre_insert(&mut self, world: &specs::World) {
        use common::resources::Time;
        use specs::WorldExt;

        // Time isn't synced between client and server so replace the Time from the
        // server with the Client's local Time to enable accurate comparison.
        self.last_change.time = *world.read_resource::<Time>();
    }

    fn pre_modify(&mut self, world: &specs::World) {
        use common::resources::Time;
        use specs::WorldExt;

        // Time isn't synced between client and server so replace the Time from the
        // server with the Client's local Time to enable accurate comparison.
        self.last_change.time = *world.read_resource::<Time>();
    }
}

impl NetSync for Heads {
    const SYNC_FROM: SyncFrom = SyncFrom::AnyEntity;
}

impl NetSync for Poise {
    const SYNC_FROM: SyncFrom = SyncFrom::AnyEntity;
}

impl NetSync for LightEmitter {
    const SYNC_FROM: SyncFrom = SyncFrom::AnyEntity;
}

impl NetSync for LootOwner {
    const SYNC_FROM: SyncFrom = SyncFrom::AnyEntity;
}

impl NetSync for PickupItem {
    const SYNC_FROM: SyncFrom = SyncFrom::AnyEntity;
}

impl NetSync for ThrownItem {
    const SYNC_FROM: SyncFrom = SyncFrom::AnyEntity;
}

impl NetSync for Scale {
    const SYNC_FROM: SyncFrom = SyncFrom::AnyEntity;
}

impl NetSync for Group {
    const SYNC_FROM: SyncFrom = SyncFrom::AnyEntity;
}

impl NetSync for IsMount {
    const SYNC_FROM: SyncFrom = SyncFrom::AnyEntity;
}

impl NetSync for IsRider {
    const SYNC_FROM: SyncFrom = SyncFrom::AnyEntity;
}

impl NetSync for IsVolumeRider {
    const SYNC_FROM: SyncFrom = SyncFrom::AnyEntity;
}

impl NetSync for VolumeRiders {
    const SYNC_FROM: SyncFrom = SyncFrom::AnyEntity;
}

impl NetSync for IsLeader {
    const SYNC_FROM: SyncFrom = SyncFrom::AnyEntity;
}

impl NetSync for IsFollower {
    const SYNC_FROM: SyncFrom = SyncFrom::AnyEntity;
}

impl NetSync for Mass {
    const SYNC_FROM: SyncFrom = SyncFrom::AnyEntity;
}

impl NetSync for Density {
    const SYNC_FROM: SyncFrom = SyncFrom::AnyEntity;
}

impl NetSync for Collider {
    const SYNC_FROM: SyncFrom = SyncFrom::AnyEntity;
}

impl NetSync for Sticky {
    const SYNC_FROM: SyncFrom = SyncFrom::AnyEntity;
}

impl NetSync for Immovable {
    const SYNC_FROM: SyncFrom = SyncFrom::AnyEntity;
}

impl NetSync for CharacterState {
    const SYNC_FROM: SyncFrom = SyncFrom::AnyEntity;
}

impl NetSync for CharacterActivity {
    const SYNC_FROM: SyncFrom = SyncFrom::AnyEntity;
}

impl NetSync for Shockwave {
    const SYNC_FROM: SyncFrom = SyncFrom::AnyEntity;
}

impl NetSync for Beam {
    const SYNC_FROM: SyncFrom = SyncFrom::AnyEntity;
}

impl NetSync for Alignment {
    const SYNC_FROM: SyncFrom = SyncFrom::AnyEntity;
}

impl NetSync for Inventory {
    const SYNC_FROM: SyncFrom = SyncFrom::AnyEntity;
}

impl NetSync for SkillSet {
    const SYNC_FROM: SyncFrom = SyncFrom::AnyEntity;
}

impl NetSync for Stance {
    const SYNC_FROM: SyncFrom = SyncFrom::AnyEntity;
}

impl NetSync for Object {
    const SYNC_FROM: SyncFrom = SyncFrom::AnyEntity;
}

impl NetSync for FrontendMarker {
    const SYNC_FROM: SyncFrom = SyncFrom::AnyEntity;
}

impl NetSync for Arcing {
    const SYNC_FROM: SyncFrom = SyncFrom::AnyEntity;
}

impl NetSync for ConcealedUnlessTrueSight {
    const SYNC_FROM: SyncFrom = SyncFrom::AnyEntity;
}

// A disguise's whole purpose is to be seen (as a disguise) by everyone
// nearby — unlike `RemoteSense`/`Detected` below, there is no "who is
// watching what" leak to protect against here.
impl NetSync for Disguise {
    const SYNC_FROM: SyncFrom = SyncFrom::AnyEntity;
}

// These are synced only from the client's own entity.

impl NetSync for Admin {
    const SYNC_FROM: SyncFrom = SyncFrom::ClientEntity;
}

impl NetSync for Combo {
    const SYNC_FROM: SyncFrom = SyncFrom::ClientSpectatorEntity;
}

impl NetSync for ActiveAbilities {
    const SYNC_FROM: SyncFrom = SyncFrom::ClientSpectatorEntity;
}

impl NetSync for AbilityCooldowns {
    const SYNC_FROM: SyncFrom = SyncFrom::ClientEntity;
}

// Owner-private, and deliberately not `ClientSpectatorEntity`: an armed
// trigger is information a PvP opponent would pay for.
impl NetSync for TriggerSlots {
    const SYNC_FROM: SyncFrom = SyncFrom::ClientEntity;
}

// Owner-private, same reasoning as `TriggerSlots` above.
impl NetSync for SpellMastery {
    const SYNC_FROM: SyncFrom = SyncFrom::ClientEntity;
}

impl NetSync for AttunedItems {
    const SYNC_FROM: SyncFrom = SyncFrom::ClientEntity;
}

// Owner-private: see the comment on `remote_sense` in `synced_components!`
// above for why this must never become `SyncFrom::AnyEntity` (a PvP
// information leak) nor `SyncFrom::ClientSpectatorEntity` (the opposite sync
// direction — that scope sends the *spectated* entity's own components to the
// spectator, not who is spectating it).
impl NetSync for RemoteSense {
    const SYNC_FROM: SyncFrom = SyncFrom::ClientEntity;
}

// Owner-private: see the comment on `detected` in `synced_components!` above
// for why this must never become `SyncFrom::AnyEntity`.
impl NetSync for Detected {
    const SYNC_FROM: SyncFrom = SyncFrom::ClientEntity;
}

impl NetSync for AbilityPool {
    const SYNC_FROM: SyncFrom = SyncFrom::ClientEntity;
}

impl NetSync for CanBuild {
    const SYNC_FROM: SyncFrom = SyncFrom::ClientEntity;
}

impl NetSync for IsInteractor {
    const SYNC_FROM: SyncFrom = SyncFrom::ClientSpectatorEntity;
}

impl NetSync for Interactors {
    const SYNC_FROM: SyncFrom = SyncFrom::ClientSpectatorEntity;
}
