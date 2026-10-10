//! The technical ceiling on tracked rtsim NPCs (NH-166 T36-ceiling,
//! NH-171): a safety limit the architect never spawns past, not a
//! population target (the base target is 6 000).

use common::rtsim::Role;
use tracing::{info, warn};

use crate::Data;

/// The default technical ceiling on tracked rtsim NPCs (NH-171): a safety
/// limit, not a population target (the base target is 6 000).
pub const DEFAULT_NPC_CEILING: u32 = 10_000;
/// Overrides [`DEFAULT_NPC_CEILING`] (a positive integer).
pub const NPC_CEILING_ENV: &str = "RTSIM_NPC_CEILING";

impl NpcCeiling {
    /// [`DEFAULT_NPC_CEILING`], or [`NPC_CEILING_ENV`] when it holds a
    /// positive integer.
    pub fn from_env() -> Self {
        let limit = match std::env::var(NPC_CEILING_ENV) {
            Err(_) => DEFAULT_NPC_CEILING,
            Ok(value) => match value.trim().parse::<u32>() {
                Ok(limit) if limit > 0 => limit,
                _ => {
                    warn!(
                        %value,
                        default = DEFAULT_NPC_CEILING,
                        "{NPC_CEILING_ENV} is not a positive integer; using the default"
                    );
                    DEFAULT_NPC_CEILING
                },
            },
        };
        info!(limit, "rtsim NPC ceiling");
        Self::new(limit)
    }
}

/// Where the tracked NPC count stands against the ceiling.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CeilingZone {
    Below,
    /// At or above 90% of the ceiling: still spawning, with a warning.
    Warning,
    /// At or above the ceiling: no new tracked NPCs.
    Full,
}

/// The technical ceiling guard (NH-166 T36-ceiling, NH-171).
///
/// The architect asks it how many NPCs it may create this round. At or over
/// the ceiling the answer is 0: nothing new is created, and nothing that
/// exists is touched -- living NPCs stay, and the queued respawns stay queued
/// (and saved) until there is room again. Logs once per zone change, not
/// every tick.
#[derive(Clone, Debug)]
pub struct NpcCeiling {
    limit: u32,
    zone: CeilingZone,
}

impl NpcCeiling {
    pub fn new(limit: u32) -> Self {
        Self {
            limit,
            zone: CeilingZone::Below,
        }
    }

    /// 90% of the limit, rounded up.
    pub fn warning_at(&self) -> u32 { self.limit - self.limit / 10 }

    pub fn zone_of(&self, tracked: u32) -> CeilingZone {
        if tracked >= self.limit {
            CeilingZone::Full
        } else if tracked >= self.warning_at() {
            CeilingZone::Warning
        } else {
            CeilingZone::Below
        }
    }

    /// How many new tracked NPCs may be created when `tracked` exist now.
    pub fn allowance(&mut self, tracked: u32) -> u32 {
        let zone = self.zone_of(tracked);
        if zone != self.zone {
            match zone {
                CeilingZone::Full => warn!(
                    tracked,
                    limit = self.limit,
                    "rtsim NPC ceiling reached: no new NPCs are created until the count drops \
                     (existing NPCs are kept)"
                ),
                CeilingZone::Warning => warn!(
                    tracked,
                    limit = self.limit,
                    "rtsim NPC count is above 90% of the ceiling"
                ),
                CeilingZone::Below => info!(
                    tracked,
                    limit = self.limit,
                    "rtsim NPC count is back below 90% of the ceiling"
                ),
            }
            self.zone = zone;
        }
        self.limit.saturating_sub(tracked)
    }
}

/// The NPCs the ceiling counts: every rtsim NPC except vehicles (airships,
/// which are not population). Players are not NPCs.
pub fn tracked_npc_count(data: &Data) -> u32 {
    data.actors
        .values()
        .filter(|actor| actor.npc().is_some() && !matches!(actor.role, Role::Vehicle))
        .count() as u32
}

#[cfg(test)]
mod tests {
    use super::{
        super::{MIN_SPAWN_DELAY, respawn_queued},
        *,
    };
    use crate::data::{
        Actor, CURRENT_VERSION, Nature,
        architect::{Death, TrackedPopulation},
        nature,
    };
    use common::{
        comp::{self, Body},
        grid::Grid,
        resources::TimeOfDay,
        rtsim::Profession,
    };
    use vek::{Vec2, Vec3};

    fn data_with_npcs(npcs: u32, vehicles: u32) -> Data {
        let mut data = Data {
            version: CURRENT_VERSION,
            nature: Nature {
                chunks: Grid::populate_from(Vec2::new(1, 1), |_| nature::Chunk {
                    res: Default::default(),
                }),
            },
            actors: Default::default(),
            sites: Default::default(),
            factions: Default::default(),
            reports: Default::default(),
            architect: Default::default(),
            quests: Default::default(),
            banished: Default::default(),
            undercompact_gate: Default::default(),
            terrain_overrides: Default::default(),
            tick: 0,
            time_of_day: TimeOfDay(10.0 * MIN_SPAWN_DELAY),
            should_purge: false,
            authored_rasters_digest: None,
            authored_region_digests: Default::default(),
            airship_sim: Default::default(),
        };
        for i in 0..npcs {
            data.spawn_actor(Actor::new_npc(i, Vec3::zero(), guard_body(), guard_role()));
        }
        for i in 0..vehicles {
            data.spawn_actor(Actor::new_npc(
                i,
                Vec3::zero(),
                Body::Ship(comp::body::ship::Body::DefaultAirship),
                Role::Vehicle,
            ));
        }
        data
    }

    fn guard_body() -> Body { Body::Humanoid(comp::humanoid::Body::random()) }

    fn guard_role() -> Role { Role::Civilised(Some(Profession::Guard)) }

    /// Queues `n` guard respawns that are due, and wants enough guards for
    /// all of them.
    fn queue_guards(data: &mut Data, n: u32) {
        for _ in 0..n {
            let death = Death {
                time: TimeOfDay(0.0),
                body: guard_body(),
                role: guard_role(),
                faction: None,
            };
            data.architect.deaths.push_back(death);
        }
        data.architect
            .wanted_population
            .add(TrackedPopulation::Guards, 1_000_000);
    }

    /// One architect round: the ceiling's allowance, then the queue drain
    /// with a spawner that always succeeds.
    fn round(data: &mut Data, ceiling: &mut NpcCeiling, count_to_spawn: u32) -> u32 {
        let allowance = ceiling.allowance(tracked_npc_count(data));
        let tod = data.time_of_day;
        let mut spawned = 0;
        respawn_queued(data, tod, count_to_spawn.min(allowance), |data, death| {
            data.spawn_actor(Actor::new_npc(
                0,
                Vec3::zero(),
                death.body,
                death.role.clone(),
            ));
            spawned += 1;
            true
        });
        spawned
    }

    #[test]
    fn ceiling_zones_and_allowance() {
        let mut ceiling = NpcCeiling::new(10_000);
        assert_eq!(ceiling.warning_at(), 9_000);
        assert_eq!(ceiling.zone_of(0), CeilingZone::Below);
        assert_eq!(ceiling.zone_of(8_999), CeilingZone::Below);
        assert_eq!(ceiling.zone_of(9_000), CeilingZone::Warning);
        assert_eq!(ceiling.zone_of(9_999), CeilingZone::Warning);
        assert_eq!(ceiling.zone_of(10_000), CeilingZone::Full);
        assert_eq!(ceiling.zone_of(12_345), CeilingZone::Full);

        assert_eq!(ceiling.allowance(6_000), 4_000);
        assert_eq!(ceiling.allowance(9_500), 500);
        assert_eq!(ceiling.allowance(10_000), 0);
        assert_eq!(ceiling.allowance(10_001), 0);
        // And back: the guard is not latched.
        assert_eq!(ceiling.allowance(8_000), 2_000);
        assert_eq!(ceiling.zone, CeilingZone::Below);

        // Small limits round the warning threshold up (90% of 15 = 13.5).
        assert_eq!(NpcCeiling::new(15).warning_at(), 14);
    }

    #[test]
    fn vehicles_do_not_count() {
        assert_eq!(tracked_npc_count(&data_with_npcs(7, 3)), 7);
    }

    #[test]
    fn below_the_warning_zone_spawns_normally() {
        let mut data = data_with_npcs(50, 2);
        queue_guards(&mut data, 10);
        let mut ceiling = NpcCeiling::new(100);
        assert_eq!(round(&mut data, &mut ceiling, 10), 10);
        assert_eq!(tracked_npc_count(&data), 60);
        assert!(data.architect.deaths.is_empty());
    }

    #[test]
    fn in_the_warning_zone_spawns_only_up_to_the_ceiling() {
        let mut data = data_with_npcs(95, 0);
        queue_guards(&mut data, 10);
        let mut ceiling = NpcCeiling::new(100);
        assert_eq!(round(&mut data, &mut ceiling, 10), 5);
        assert_eq!(ceiling.zone, CeilingZone::Warning);
        assert_eq!(tracked_npc_count(&data), 100);
        // The rest stay queued for later.
        assert_eq!(data.architect.deaths.len(), 5);
    }

    #[test]
    fn at_and_over_the_ceiling_nothing_is_created_or_removed() {
        for existing in [100, 130] {
            let mut data = data_with_npcs(existing, 4);
            queue_guards(&mut data, 10);
            let ids_before: Vec<_> = data.actors.keys().collect();
            let mut ceiling = NpcCeiling::new(100);

            for _ in 0..5 {
                assert_eq!(round(&mut data, &mut ceiling, 19), 0);
            }
            assert_eq!(ceiling.zone, CeilingZone::Full);
            // Existing NPCs (and vehicles) are untouched ...
            assert_eq!(data.actors.keys().collect::<Vec<_>>(), ids_before);
            // ... and so is the saved respawn queue.
            assert_eq!(data.architect.deaths.len(), 10);

            // The save still round-trips.
            let mut bytes = Vec::new();
            data.write_to(&mut bytes).unwrap();
            let Ok(loaded) = Data::from_reader(&bytes[..]) else {
                panic!("the save must load");
            };
            assert_eq!(loaded.actors.len(), data.actors.len());
            assert_eq!(loaded.architect.deaths.len(), 10);
        }
    }
}
