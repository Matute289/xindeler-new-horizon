//! Server-side resolution of [`BanishEvent`]: the authoritative saving throw,
//! the persisted record, the frozen/limbo marker, and the
//! `DestroyEvent{Banished}` that pays the fractional reward.
//!
//! Everything but the no-op below is `worldgen`-only: without rtsim there is no
//! `Banishments` registry to record a banishment in and no persistent world to
//! bring the creature back to.

use common::event::BanishEvent;

use super::ServerEvent;

/// Without `worldgen` there is no rtsim and no persistent world, so a
/// banishment can neither be recorded durably nor honoured later. Draining the
/// events without acting on them is the only correct behaviour — and it keeps
/// `check_event_handlers` satisfied (exactly one handler per event).
#[cfg(not(feature = "worldgen"))]
impl ServerEvent for BanishEvent {
    type SystemData<'a> = ();

    fn handle(_events: impl ExactSizeIterator<Item = Self>, (): Self::SystemData<'_>) {}
}

#[cfg(feature = "worldgen")]
use common::{
    assets::{AssetExt, Ron},
    combat::{self, RemovalInfo},
    comp::{
        self, Alignment, Banished, Body, Energy, Group, Health, Inventory, Poise, Pos, Scale,
        SkillSet, Stats, agent::Agent, inventory::item::MaterialStatManifest,
    },
    event::{DestroyEvent, EventBus},
    outcome::Outcome,
    resources::Time,
    terrain::CoordinateConversions,
    uid::{IdMaps, Uid},
};
#[cfg(feature = "worldgen")]
use rand::{Rng, RngExt};
#[cfg(feature = "worldgen")]
use rtsim::data::{BanishedCreature, BanishedKind};
#[cfg(feature = "worldgen")]
use specs::{Entities, Read, ReadExpect, ReadStorage, SystemData, WriteStorage, shred};

/// Uniformly draws a wall-clock return deadline inside the authored window.
/// Tolerates a degenerate or inverted window (hand-edited RON) by collapsing
/// to the lower bound rather than panicking inside `random_range`.
#[cfg(feature = "worldgen")]
fn roll_return_deadline(
    now_unix_secs: u64,
    min_return_hours: f64,
    max_return_hours: f64,
    rng: &mut impl Rng,
) -> u64 {
    let min_secs = (min_return_hours.max(0.0) * 3600.0) as u64;
    let max_secs = (max_return_hours.max(0.0) * 3600.0) as u64;
    let delay = if max_secs > min_secs {
        rng.random_range(min_secs..=max_secs)
    } else {
        min_secs
    };
    now_unix_secs.saturating_add(delay)
}

#[cfg(feature = "worldgen")]
use crate::banishment::death_forestalls_banishment;

#[cfg(feature = "worldgen")]
#[derive(SystemData)]
pub struct BanishEventData<'a> {
    entities: Entities<'a>,
    /// 🔴 `ReadExpect`, not `WriteExpect`, is deliberate — `with_banishments`
    /// takes `&self` because `RtState::data_mut` is interior-mutable
    /// (`rtsim/src/lib.rs`). **That makes shared access a scheduler
    /// invariant, not a free choice:** this is currently the only
    /// `ReadExpect<RtSim>` in the crate (every other consumer takes
    /// `WriteExpect`, which specs serialises). A *second* in-dispatcher
    /// `ReadExpect<RtSim>` that also mutates would be free to run in parallel
    /// with this one, and the inner `AtomicRefCell::borrow_mut` would panic at
    /// runtime. If one is ever added, promote both to `WriteExpect` — the
    /// `DestroyEvent` dependency edge already serialises this handler, so it
    /// costs no throughput.
    rtsim: ReadExpect<'a, crate::rtsim::RtSim>,
    id_maps: Read<'a, IdMaps>,
    msm: ReadExpect<'a, MaterialStatManifest>,
    time: Read<'a, Time>,
    outcomes: Read<'a, EventBus<Outcome>>,
    destroys: Read<'a, EventBus<DestroyEvent>>,
    banished: WriteStorage<'a, Banished>,
    uids: ReadStorage<'a, Uid>,
    positions: ReadStorage<'a, Pos>,
    bodies: ReadStorage<'a, Body>,
    scales: ReadStorage<'a, Scale>,
    alignments: ReadStorage<'a, Alignment>,
    stats: ReadStorage<'a, Stats>,
    healths: ReadStorage<'a, Health>,
    energies: ReadStorage<'a, Energy>,
    poises: ReadStorage<'a, Poise>,
    inventories: ReadStorage<'a, Inventory>,
    skill_sets: ReadStorage<'a, SkillSet>,
    groups: ReadStorage<'a, Group>,
    agents: ReadStorage<'a, Agent>,
    rtsim_actors: ReadStorage<'a, common::rtsim::ActorId>,
}

#[cfg(feature = "worldgen")]
impl ServerEvent for BanishEvent {
    type SystemData<'a> = BanishEventData<'a>;

    fn handle(events: impl ExactSizeIterator<Item = Self>, mut data: Self::SystemData<'_>) {
        let mut outcomes = data.outcomes.emitter();
        let mut destroys = data.destroys.emitter();
        let mut rng = rand::rng();

        for ev in events {
            if !data.entities.is_alive(ev.entity) || data.banished.contains(ev.entity) {
                continue;
            }
            let Some(caster) = data.id_maps.uid_entity(ev.banished_by) else {
                continue;
            };
            let Some(caster_stats) = data.stats.get(caster) else {
                continue;
            };
            let (
                Some(target_uid),
                Some(target_pos),
                Some(target_body),
                Some(target_health),
                Some(target_energy),
                Some(target_poise),
                Some(target_inventory),
                Some(target_skill_set),
            ) = (
                data.uids.get(ev.entity).copied(),
                data.positions.get(ev.entity).copied(),
                data.bodies.get(ev.entity).copied(),
                data.healths.get(ev.entity),
                data.energies.get(ev.entity),
                data.poises.get(ev.entity),
                data.inventories.get(ev.entity),
                data.skill_sets.get(ev.entity),
            )
            else {
                continue;
            };
            // A creature that is dead — or that reached zero HP this tick and
            // is only waiting for `DestroyEvent` to say so — cannot be
            // banished: the record would outlive the entity and either
            // rehydrate a creature that was killed or sit orphaned forever.
            if death_forestalls_banishment(target_health) {
                continue;
            }

            // --- the saving throw (spec §4) -------------------------------
            let tuning = Ron::<combat::CombatTuning>::load_expect("common.combat_tuning").read();
            let combat_rating = combat::combat_rating(
                target_inventory,
                target_health,
                target_energy,
                target_poise,
                target_skill_set,
                target_body,
                &data.msm,
            );
            let target_stats = data.stats.get(ev.entity);
            let target_info = combat::SaveTargetInfo {
                stats_magic_evasion: target_stats.map_or(0.0, |s| s.magic_evasion),
                crowd_control_resistance: target_stats.map_or(0.0, |s| s.crowd_control_resistance),
                stats_magic_resistance: target_stats.map_or(0.0, |s| s.magic_resistance),
                magic_resist_tier: target_body.magic_resist_tier(),
                combat_rating,
            };
            let ctx = combat::SaveCombatContext {
                caster_uid: ev.banished_by,
                caster_group: data.groups.get(caster).copied(),
                target_uid,
                target_group: data.groups.get(ev.entity).copied(),
                target_hostile_focus: data
                    .agents
                    .get(ev.entity)
                    .and_then(|agent| agent.target)
                    .filter(|target| target.hostile)
                    .and_then(|target| {
                        data.uids
                            .get(target.target)
                            .map(|uid| (*uid, data.groups.get(target.target).copied()))
                    }),
                target_last_change: Some(&target_health.last_change),
                caster_last_change: data.healths.get(caster).map(|h| &h.last_change),
                now: data.time.0,
            };
            let chance = combat::saving_throw_chance(
                &combat::SaveCasterInfo {
                    magic_accuracy: caster_stats.magic_accuracy,
                },
                &target_info,
                combat::is_fighting_caster(&ctx),
                &tuning.0,
            );
            if rng.random::<f32>() >= chance {
                // Saved. Same feedback the charm path already produces, so
                // nothing new is needed on the client.
                outcomes.emit(Outcome::Resisted {
                    pos: target_pos.0,
                    target: target_uid,
                });
                continue;
            }

            // --- record it durably, then mark the entity ------------------
            let returns_at_unix_secs = roll_return_deadline(
                crate::banishment::now_unix_secs(),
                ev.min_return_hours,
                ev.max_return_hours,
                &mut rng,
            );
            // The rtsim-vs-plain-mob fork. An rtsim actor keeps living in
            // `data.actors` with `presence = None` (task N38B21-H's park pass
            // clears it), so the record only names it; a plain mob is
            // remembered by nothing else, so the record carries its whole
            // archetype.
            let kind = match data.rtsim_actors.get(ev.entity).copied() {
                Some(actor) => BanishedKind::RtsimActor(actor),
                None => BanishedKind::Freestanding {
                    body: target_body,
                    // `Owned(Uid)` is a *session* handle: `UidAllocator`
                    // restarts at 1 on every server boot, so persisting a raw
                    // `Owned(uid)` would let a banished pet rehydrate "owned"
                    // by an unrelated entity after a restart. A returning pet
                    // comes back tame and ownerless instead.
                    alignment: match data.alignments.get(ev.entity).copied() {
                        Some(Alignment::Owned(_)) => Alignment::Tame,
                        Some(alignment) => alignment,
                        None => Alignment::Wild,
                    },
                    creature_kind: target_stats.and_then(|s| s.creature_kind),
                    scale: data.scales.get(ev.entity).map_or(1.0, |s| s.0),
                },
            };
            let record = BanishedCreature {
                kind,
                return_pos: target_pos.0,
                return_chunk: target_pos.0.xy().as_::<i32>().wpos_to_cpos(),
                returns_at_unix_secs,
            };
            let id = data
                .rtsim
                .with_banishments(|banishments| banishments.insert(record));

            let _ = data.banished.insert(ev.entity, Banished {
                id,
                return_pos: target_pos.0,
                returns_at_unix_secs,
            });

            // Rewards and the "not a kill, not deleted" semantics both live in
            // the `DestroyEvent` handler, so a banishment reuses the shipped
            // XP/loot machinery verbatim instead of copying it.
            destroys.emit(DestroyEvent {
                entity: ev.entity,
                cause: comp::HealthChange {
                    amount: 0.0,
                    by: Some(combat::DamageContributor::Solo(ev.banished_by)),
                    cause: Some(combat::DamageSource::Other),
                    magic_source: None,
                    time: *data.time,
                    precise: false,
                    instance: rng.random(),
                },
                removal: RemovalInfo::banished(ev.reward_fraction),
            });
        }
    }
}

#[cfg(all(test, feature = "worldgen"))]
mod tests {
    use super::*;

    #[test]
    fn a_return_deadline_lands_inside_the_authored_window() {
        let mut rng = rand::rng();
        let now = 1_700_000_000u64;
        for _ in 0..1000 {
            let deadline = roll_return_deadline(now, 24.0, 168.0, &mut rng);
            assert!(deadline >= now + 24 * 3600, "{deadline} too soon");
            assert!(deadline <= now + 168 * 3600, "{deadline} too late");
        }
    }

    /// A degenerate or inverted window in a hand-edited RON must not panic
    /// `rng.random_range` — it collapses to the lower bound instead.
    #[test]
    fn a_degenerate_window_collapses_to_its_lower_bound() {
        let mut rng = rand::rng();
        let now = 1_700_000_000u64;
        assert_eq!(
            roll_return_deadline(now, 24.0, 24.0, &mut rng),
            now + 24 * 3600
        );
        assert_eq!(
            roll_return_deadline(now, 24.0, 1.0, &mut rng),
            now + 24 * 3600
        );
    }
}
