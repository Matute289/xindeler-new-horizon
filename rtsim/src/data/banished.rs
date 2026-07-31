use common::{
    comp::{Alignment, BanishmentId, Body, CreatureKind},
    rtsim::ActorId,
};
use hashbrown::HashMap;
use serde::{Deserialize, Serialize};
use vek::{Vec2, Vec3};

/// What kind of thing was banished. This is the fork between a plain world
/// mob (which nothing else in the game remembers, so the record must carry
/// its whole archetype) and an rtsim actor (which rtsim already persists in
/// full, so the record only has to name it).
#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum BanishedKind {
    /// A plain world mob, spawned by worldgen's chunk supplement and carrying
    /// no `rtsim::ActorId`.
    Freestanding {
        body: Body,
        alignment: Alignment,
        /// `Stats::creature_kind` at the moment of banishment, so an authored
        /// reskin (`EntityConfig::creature_type`) survives the round trip
        /// instead of silently reverting to `body.creature_kind()`.
        #[serde(default)]
        creature_kind: Option<CreatureKind>,
        scale: f32,
    },
    /// An rtsim actor. Its `Actor` stays in `data.actors` the whole time, with
    /// `presence = None`; nothing about its body, loadout or history needs
    /// duplicating here.
    RtsimActor(ActorId),
}

/// Everything needed to bring a banished creature back, in particular after a
/// server restart, when no ECS entity survives.
///
/// A flat archetype (for the `Freestanding` case), not a snapshot of the live
/// entity: the design says a returning creature is **fully reset**, so buffs,
/// mid-fight health and aggro are intentionally not persisted. Shape mirrors
/// `architect::Death` (a `Body` plus a timestamp), plus the position the
/// creature has to come back to.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct BanishedCreature {
    pub kind: BanishedKind,
    pub return_pos: Vec3<f32>,
    /// `return_pos`'s chunk key, precomputed at banishment time so worldgen's
    /// respawn suppression runs on every chunk load without recomputing a key
    /// per record.
    pub return_chunk: Vec2<i32>,
    /// Wall-clock seconds since the UNIX epoch — see `comp::Banished`'s doc
    /// comment for why neither `Time` nor `TimeOfDay` works here.
    pub returns_at_unix_secs: u64,
}

impl BanishedCreature {
    /// Whether *worldgen* (rather than rtsim) is what would put this creature
    /// back in the world — i.e. whether it needs a rehydrated entity and a
    /// suppressed chunk spawn.
    ///
    /// Exhaustive on purpose: a future `BanishedKind` variant must make this
    /// decision explicitly rather than silently defaulting either way.
    pub fn is_freestanding(&self) -> bool {
        match &self.kind {
            BanishedKind::Freestanding { .. } => true,
            BanishedKind::RtsimActor(_) => false,
        }
    }

    /// The effective `CreatureKind` this creature had when it was banished:
    /// the authored `EntityConfig::creature_type` override if there was one,
    /// else the body's own default. This is the *same* formula
    /// `SpawnEntityData::from_entity_info` resolves at spawn time
    /// (`comp::Stats::new` seeds `creature_kind` from the body, then the
    /// override wins), which is what makes the two comparable.
    ///
    /// `None` for an rtsim actor: worldgen never spawns those, so they never
    /// contribute a suppression.
    pub fn freestanding_creature_kind(&self) -> Option<Option<CreatureKind>> {
        match &self.kind {
            BanishedKind::Freestanding {
                body,
                creature_kind,
                ..
            } => Some(creature_kind.or_else(|| body.creature_kind())),
            BanishedKind::RtsimActor(_) => None,
        }
    }
}

/// Every creature currently banished, persisted inside rtsim's save file so a
/// server restart mid-banishment does not lose them.
///
/// Deliberately independent of `Actors`: almost every banishable target is a
/// plain world mob with no rtsim actor at all, so this rides on rtsim's
/// *persistence*, not on its NPC model.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Banishments {
    /// Monotonic id source. Persisted, so ids keep climbing across restarts
    /// and a rehydrated record can never collide with a fresh banishment.
    next_id: BanishmentId,
    creatures: HashMap<BanishmentId, BanishedCreature>,
    /// Ids with no live ECS entity: every record right after a load, plus any
    /// whose parked entity was lost mid-session. Drained by
    /// `server::banishment::maintain`, which spawns a fresh frozen entity for
    /// each one. Rebuilt on load by `prepare` rather than persisted — the
    /// same `#[serde(skip)]` + `prepare()` shape `Quests::related_quests`
    /// uses.
    #[serde(skip)]
    pending_rehydration: Vec<BanishmentId>,
}

impl Banishments {
    /// Records a newly banished creature and returns its key, which the
    /// caller stores on the entity's `comp::Banished`.
    pub fn insert(&mut self, creature: BanishedCreature) -> BanishmentId {
        let id = self.next_id;
        self.next_id = self.next_id.wrapping_add(1);
        self.creatures.insert(id, creature);
        id
    }

    pub fn get(&self, id: BanishmentId) -> Option<&BanishedCreature> { self.creatures.get(&id) }

    /// Forgets a banishment — called once the creature has actually returned.
    pub fn remove(&mut self, id: BanishmentId) -> Option<BanishedCreature> {
        self.pending_rehydration.retain(|pending| *pending != id);
        self.creatures.remove(&id)
    }

    /// Marks a record as needing a fresh frozen entity. Idempotent, and a
    /// no-op for an id with no record.
    pub fn queue_rehydration(&mut self, id: BanishmentId) {
        if self.creatures.contains_key(&id) && !self.pending_rehydration.contains(&id) {
            self.pending_rehydration.push(id);
        }
    }

    pub fn take_pending_rehydration(&mut self) -> Vec<BanishmentId> {
        std::mem::take(&mut self.pending_rehydration)
    }

    /// One entry per `Freestanding` creature currently banished from `chunk`,
    /// carrying that creature's effective `CreatureKind`. Worldgen's chunk
    /// supplement subtracts these when it loads that chunk.
    ///
    /// 🔴 This is a **headcount**, not an identity list, and that is
    /// deliberate. Worldgen cannot be asked to leave *the same individual*
    /// out: `world/src/lib.rs`'s `dynamic_rng` is reseeded per generation
    /// (`ChaCha8Rng::from_seed(rand::rng().random())`, commented "Only use
    /// for rng affecting dynamic elements like chests and entities!"), so
    /// `apply_wildlife_supplement` rolls a **fresh random population** on
    /// every chunk load and the banished individual is never guaranteed to
    /// reappear. Matching on the exact `Body` therefore succeeded only by
    /// coincidence — and never at all for bodies with randomised appearance
    /// fields, which are part of the derived `PartialEq`.
    ///
    /// The invariant that actually matters is a *count* one: while a creature
    /// is away it is still owed a return, so the chunk must carry one fewer
    /// creature of its kind, or the return makes it a net +1. Counting is
    /// immune to the non-deterministic roll. The `CreatureKind` filter keeps
    /// a banished fiend from suppressing an unrelated deer.
    ///
    /// The inner `Option` is the creature kind itself — a mob whose body has
    /// no `CreatureKind` at all (reachable through `/banish`) suppresses a
    /// candidate that likewise has none.
    ///
    /// rtsim actors are excluded: worldgen never spawns them, and their own
    /// re-materialisation is suppressed deterministically via `presence`.
    pub fn freestanding_suppressions_in_chunk(
        &self,
        chunk: Vec2<i32>,
    ) -> Vec<Option<CreatureKind>> {
        self.creatures
            .values()
            .filter(|creature| creature.return_chunk == chunk)
            .filter_map(|creature| creature.freestanding_creature_kind())
            .collect()
    }

    /// `RtsimActor` records whose deadline has arrived. These are the ones the
    /// return pass cannot discover by joining on the `Banished` component,
    /// because parking an rtsim actor deletes its ECS entity outright.
    pub fn due_rtsim_actors(
        &self,
        now_unix_secs: u64,
    ) -> impl Iterator<Item = (BanishmentId, ActorId)> + '_ {
        self.creatures
            .iter()
            .filter(move |(_, creature)| creature.returns_at_unix_secs <= now_unix_secs)
            .filter_map(|(id, creature)| match creature.kind {
                BanishedKind::RtsimActor(actor) => Some((*id, actor)),
                BanishedKind::Freestanding { .. } => None,
            })
    }

    pub fn len(&self) -> usize { self.creatures.len() }

    pub fn is_empty(&self) -> bool { self.creatures.is_empty() }

    /// Post-load rehydration hook: nothing in the ECS survives a restart, so
    /// every persisted `Freestanding` record needs a fresh frozen entity.
    /// Called from `Data::prepare`.
    ///
    /// `RtsimActor` records are deliberately **not** queued: rtsim already
    /// persists those actors in full, their `presence` is still `None` in the
    /// loaded save, and rtsim's own load loop brings them back the moment the
    /// return pass restores it. Queueing them would spawn a duplicate.
    pub fn prepare(&mut self) {
        self.pending_rehydration = self
            .creatures
            .iter()
            .filter(|(_, creature)| creature.is_freestanding())
            .map(|(id, _)| *id)
            .collect();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use common::{
        comp::{Alignment, Body, CreatureKind, bird_large},
        rtsim::ActorId,
    };
    use vek::{Vec2, Vec3};

    fn phoenix_body() -> Body {
        Body::BirdLarge(bird_large::Body {
            species: bird_large::Species::Phoenix,
            body_type: bird_large::BodyType::Female,
        })
    }

    /// A plain world mob: the record carries the whole archetype, because
    /// nothing else in the game remembers this creature.
    fn phoenix(returns_at_unix_secs: u64) -> BanishedCreature {
        BanishedCreature {
            kind: BanishedKind::Freestanding {
                body: phoenix_body(),
                alignment: Alignment::Enemy,
                creature_kind: Some(CreatureKind::Celestial),
                scale: 1.0,
            },
            return_pos: Vec3::new(512.0, 512.0, 90.0),
            return_chunk: Vec2::new(1, 1),
            returns_at_unix_secs,
        }
    }

    /// An rtsim actor: rtsim already persists everything about it, so the
    /// record only records which actor is away.
    fn rtsim_phoenix(actor: ActorId, returns_at_unix_secs: u64) -> BanishedCreature {
        BanishedCreature {
            kind: BanishedKind::RtsimActor(actor),
            return_pos: Vec3::new(512.0, 512.0, 90.0),
            return_chunk: Vec2::new(1, 1),
            returns_at_unix_secs,
        }
    }

    /// `ActorId` is an opaque slotmap key, so mint real ones from a throwaway
    /// `SlotMap` rather than transmuting an integer — the same trick
    /// `data::sentiment`'s tests use.
    fn actor_ids<const N: usize>() -> [ActorId; N] {
        let mut ids = slotmap::SlotMap::<ActorId, ()>::default();
        std::array::from_fn(|_| ids.insert(()))
    }

    #[test]
    fn ids_are_unique_and_records_are_retrievable() {
        let mut banishments = Banishments::default();
        let a = banishments.insert(phoenix(100));
        let b = banishments.insert(phoenix(200));
        assert_ne!(a, b);
        assert_eq!(banishments.len(), 2);
        assert_eq!(banishments.get(a).unwrap().returns_at_unix_secs, 100);
        assert_eq!(banishments.get(b).unwrap().returns_at_unix_secs, 200);
    }

    #[test]
    fn removing_a_record_also_drops_it_from_the_rehydration_queue() {
        let mut banishments = Banishments::default();
        let id = banishments.insert(phoenix(100));
        banishments.queue_rehydration(id);
        assert!(banishments.remove(id).is_some());
        assert!(banishments.take_pending_rehydration().is_empty());
        assert!(banishments.is_empty());
    }

    #[test]
    fn queueing_rehydration_is_idempotent_and_ignores_unknown_ids() {
        let mut banishments = Banishments::default();
        let id = banishments.insert(phoenix(100));
        banishments.queue_rehydration(id);
        banishments.queue_rehydration(id);
        banishments.queue_rehydration(id + 999);
        assert_eq!(banishments.take_pending_rehydration(), vec![id]);
    }

    /// Nothing in the ECS survives a restart, so after a load every persisted
    /// record needs a fresh frozen entity — that queue is rebuilt by
    /// `prepare`, never persisted, exactly like `Quests::related_quests`.
    #[test]
    fn prepare_queues_every_persisted_freestanding_record_for_rehydration() {
        let [actor] = actor_ids();
        let mut banishments = Banishments::default();
        let a = banishments.insert(phoenix(100));
        let b = banishments.insert(phoenix(200));
        // An rtsim actor must NOT be queued: rtsim still persists it in full
        // and re-materialises it itself once `presence` is restored. Queueing
        // it would spawn a duplicate.
        banishments.insert(rtsim_phoenix(actor, 300));
        // Simulate a save/load: the queue is `#[serde(skip)]`, so it is empty.
        assert!(banishments.take_pending_rehydration().is_empty());
        banishments.prepare();
        let mut pending = banishments.take_pending_rehydration();
        pending.sort_unstable();
        let mut expected = vec![a, b];
        expected.sort_unstable();
        assert_eq!(pending, expected);
    }

    /// Ids must keep climbing across a restart, or a rehydrated record and a
    /// freshly banished creature can collide. Round-tripped through
    /// MessagePack because that is the codec rtsim's save file actually uses
    /// (`Data::write_to`), so this also pins that the record encodes at all.
    #[test]
    fn the_id_counter_survives_a_serde_round_trip() {
        let mut banishments = Banishments::default();
        let a = banishments.insert(phoenix(100));
        banishments.queue_rehydration(a);
        let encoded = rmp_serde::to_vec_named(&banishments).expect("serialise");
        let mut decoded: Banishments = rmp_serde::from_slice(&encoded).expect("deserialise");
        // The queue is derived state, never persisted.
        assert!(decoded.take_pending_rehydration().is_empty());
        assert_eq!(decoded.len(), 1);
        let b = decoded.insert(phoenix(200));
        assert!(b > a);
    }

    /// Task 4B subtracts these from worldgen's chunk supplement. Only
    /// `Freestanding` records count: an rtsim actor is never spawned by the
    /// chunk supplement, it is spawned by rtsim's own load loop.
    #[test]
    fn only_freestanding_records_contribute_a_suppression_for_their_chunk() {
        let [actor] = actor_ids();
        let mut banishments = Banishments::default();
        banishments.insert(phoenix(100));
        banishments.insert(rtsim_phoenix(actor, 100));
        let mut elsewhere = phoenix(100);
        elsewhere.return_chunk = Vec2::new(9, 9);
        banishments.insert(elsewhere);

        // Two phoenixes banished from (1,1), but only the freestanding one
        // was ever spawned by worldgen.
        assert_eq!(
            banishments.freestanding_suppressions_in_chunk(Vec2::new(1, 1)),
            vec![Some(CreatureKind::Celestial)]
        );
        assert_eq!(
            banishments.freestanding_suppressions_in_chunk(Vec2::new(9, 9)),
            vec![Some(CreatureKind::Celestial)]
        );
        assert!(
            banishments
                .freestanding_suppressions_in_chunk(Vec2::new(5, 5))
                .is_empty()
        );
    }

    /// The suppression must carry the creature kind the mob *actually* had,
    /// which is the authored `EntityConfig::creature_type` override when
    /// there is one — the same resolution `SpawnEntityData::from_entity_info`
    /// performs on the spawn candidate it is compared against. Reverting to
    /// `body.creature_kind()` here would make a reskinned mob suppress the
    /// wrong species.
    #[test]
    fn a_suppression_carries_the_authored_creature_kind_override() {
        let mut reskinned = phoenix(100);
        reskinned.kind = BanishedKind::Freestanding {
            body: phoenix_body(),
            alignment: Alignment::Enemy,
            creature_kind: Some(CreatureKind::Fiend),
            scale: 1.0,
        };
        let mut banishments = Banishments::default();
        banishments.insert(reskinned);

        assert_eq!(
            banishments.freestanding_suppressions_in_chunk(Vec2::new(1, 1)),
            vec![Some(CreatureKind::Fiend)],
            "the authored override must win over the phoenix body's Celestial default"
        );
    }

    /// With no override the body's own kind is used — again matching what the
    /// spawn candidate resolves to (`comp::Stats::new` seeds `creature_kind`
    /// from the body).
    #[test]
    fn a_suppression_falls_back_to_the_bodys_own_creature_kind() {
        let mut banishments = Banishments::default();
        let mut no_override = phoenix(100);
        no_override.kind = BanishedKind::Freestanding {
            body: phoenix_body(),
            alignment: Alignment::Enemy,
            creature_kind: None,
            scale: 1.0,
        };
        banishments.insert(no_override);

        assert_eq!(
            banishments.freestanding_suppressions_in_chunk(Vec2::new(1, 1)),
            vec![phoenix_body().creature_kind()]
        );
    }

    /// The return pass cannot find a banished rtsim actor by joining on
    /// `Banished`: parking one deletes its ECS entity outright.
    #[test]
    fn only_due_rtsim_actors_are_reported_for_return() {
        let [early, late] = actor_ids();
        let mut banishments = Banishments::default();
        let early_id = banishments.insert(rtsim_phoenix(early, 100));
        banishments.insert(rtsim_phoenix(late, 300));
        // A freestanding record that is also due must never show up here — it
        // is returned via its parked entity's `Banished` component instead.
        banishments.insert(phoenix(100));

        let due = banishments.due_rtsim_actors(200).collect::<Vec<_>>();
        assert_eq!(due, vec![(early_id, early)]);
        assert_eq!(banishments.due_rtsim_actors(50).count(), 0);
        assert_eq!(banishments.due_rtsim_actors(300).count(), 2);
    }

    // ── Task 4B: the worldgen respawn-suppression contract ──────────────────

    /// The suppression list must shrink the moment the creature comes back,
    /// or its chunk stays permanently one mob short.
    #[test]
    fn suppression_stops_as_soon_as_the_record_is_removed() {
        let mut banishments = Banishments::default();
        let id = banishments.insert(phoenix(100));
        assert_eq!(
            banishments
                .freestanding_suppressions_in_chunk(Vec2::new(1, 1))
                .len(),
            1
        );
        banishments.remove(id);
        assert!(
            banishments
                .freestanding_suppressions_in_chunk(Vec2::new(1, 1))
                .is_empty()
        );
    }

    /// Two creatures of the same kind banished from one chunk must suppress
    /// two spawns, not one — this is a headcount, so equal kinds must not
    /// collapse into a single entry the way a set would.
    #[test]
    fn two_banished_creatures_in_one_chunk_suppress_two_spawns() {
        let mut banishments = Banishments::default();
        banishments.insert(phoenix(100));
        banishments.insert(phoenix(200));
        assert_eq!(
            banishments.freestanding_suppressions_in_chunk(Vec2::new(1, 1)),
            vec![Some(CreatureKind::Celestial), Some(CreatureKind::Celestial)]
        );
    }
}
