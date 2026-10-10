use common::{
    comp::{self, Body},
    resources::TimeOfDay,
    rtsim::{Personality, Profession, Role},
    terrain::CoordinateConversions,
};
use rand::{
    RngExt, rng,
    seq::{IndexedRandom, IteratorRandom},
};
use vek::Vec2;
use world::{
    CONFIG, IndexRef, World, authored_raster::queries::ChunkWater, sim::SimChunk, site::SiteKind,
};

use tracing::{info, warn};

use crate::{
    Data, EventCtx, OnTick, RtState,
    data::{
        Actor,
        architect::{Death, TrackedPopulation},
    },
    event::OnDeath,
    generate::settlement_population,
};

use super::{Rule, RuleError};

/// How many ticks the architect skips.
///
/// We don't need to run it every tick.
const ARCHITECT_TICK_SKIP: u64 = 32;
/// Min spawn delay, in ingame time.
const MIN_SPAWN_DELAY: f64 = 60.0 * 60.0 * 24.0;
/// For monsters that respawn in chunks, how many chunks should we try each
/// respawn.
const RESPAWN_ATTEMPTS: usize = 30;

/// The default technical ceiling on tracked rtsim NPCs (NH-171): a safety
/// limit, not a population target (the base target is 6 000).
pub const DEFAULT_NPC_CEILING: u32 = 10_000;
/// Overrides [`DEFAULT_NPC_CEILING`] (a positive integer).
pub const NPC_CEILING_ENV: &str = "RTSIM_NPC_CEILING";

pub struct Architect {
    ceiling: NpcCeiling,
}

impl Rule for Architect {
    fn start(rtstate: &mut RtState) -> Result<Self, RuleError> {
        rtstate.bind(on_death);
        rtstate.bind(architect_tick);

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

        Ok(Self {
            ceiling: NpcCeiling::new(limit),
        })
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

    pub fn limit(&self) -> u32 { self.limit }

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

fn on_death(ctx: EventCtx<Architect, OnDeath>) {
    let data = &mut *ctx.state.data_mut();

    if let Some(actor) = data.actors.get(ctx.event.actor)
        && actor.npc().is_some()
    {
        data.architect.on_death(actor, data.time_of_day);
    }
}

fn architect_tick(ctx: EventCtx<Architect, OnTick>) {
    if !ctx.event.tick.is_multiple_of(ARCHITECT_TICK_SKIP) {
        return;
    }

    let tod = ctx.event.time_of_day;

    let data = &mut *ctx.state.data_mut();

    let mut rng = rng();
    let mut count_to_spawn = rng.random_range(1..20);
    let allowance = ctx.rule.ceiling.allowance(tracked_npc_count(data));

    let pop = data.architect.population.clone();
    'outer: for (pop, count) in pop
        .iter()
        .zip(data.architect.wanted_population.iter())
        .filter(|((_, current), (_, wanted))| current < wanted)
        .map(|((pop, current), (_, wanted))| (pop, wanted - current))
    {
        for _ in 0..count {
            let (body, role) = match pop {
                TrackedPopulation::Adventurers => (
                    Body::Humanoid(comp::humanoid::Body::random()),
                    Role::Civilised(Some(Profession::Adventurer(rng.random_range(0..=3)))),
                ),
                TrackedPopulation::Merchants => (
                    Body::Humanoid(comp::humanoid::Body::random()),
                    Role::Civilised(Some(Profession::Merchant)),
                ),
                TrackedPopulation::Guards => (
                    Body::Humanoid(comp::humanoid::Body::random()),
                    Role::Civilised(Some(Profession::Guard)),
                ),
                TrackedPopulation::Captains => (
                    Body::Humanoid(comp::humanoid::Body::random()),
                    Role::Civilised(Some(Profession::Captain)),
                ),
                TrackedPopulation::OtherTownNpcs => (
                    Body::Humanoid(comp::humanoid::Body::random()),
                    Role::Civilised(Some(match rng.random_range(0..10) {
                        0 => Profession::Hunter,
                        1 => Profession::Blacksmith,
                        2 => Profession::Chef,
                        3 => Profession::Alchemist,
                        4..=5 => Profession::Herbalist,
                        _ => Profession::Farmer,
                    })),
                ),
                TrackedPopulation::Pirates => (
                    Body::Humanoid(comp::humanoid::Body::random()),
                    Role::Civilised(Some(Profession::Pirate(false))),
                ),
                TrackedPopulation::PirateCaptains => (
                    Body::Humanoid(comp::humanoid::Body::random()),
                    Role::Civilised(Some(Profession::Pirate(true))),
                ),
                TrackedPopulation::Cultists => (
                    Body::Humanoid(comp::humanoid::Body::random()),
                    Role::Civilised(Some(Profession::Cultist)),
                ),
                TrackedPopulation::GigasFrost => (
                    Body::BipedLarge(comp::biped_large::Body::random_with(
                        &mut rng,
                        &comp::biped_large::Species::Gigasfrost,
                    )),
                    Role::Monster,
                ),
                TrackedPopulation::GigasFire => (
                    Body::BipedLarge(comp::biped_large::Body::random_with(
                        &mut rng,
                        &comp::biped_large::Species::Gigasfire,
                    )),
                    Role::Monster,
                ),
                TrackedPopulation::OtherMonsters => {
                    let species = [
                        comp::biped_large::Species::Ogre,
                        comp::biped_large::Species::Cyclops,
                        comp::biped_large::Species::Wendigo,
                        comp::biped_large::Species::Cavetroll,
                        comp::biped_large::Species::Mountaintroll,
                        comp::biped_large::Species::Swamptroll,
                        comp::biped_large::Species::Blueoni,
                        comp::biped_large::Species::Redoni,
                        comp::biped_large::Species::Tursus,
                    ]
                    .choose(&mut rng)
                    .unwrap();

                    (
                        Body::BipedLarge(comp::biped_large::Body::random_with(&mut rng, species)),
                        Role::Monster,
                    )
                },
                TrackedPopulation::CloudWyvern => (
                    Body::BirdLarge(comp::bird_large::Body::random_with(
                        &mut rng,
                        &comp::bird_large::Species::CloudWyvern,
                    )),
                    Role::Wild,
                ),
                TrackedPopulation::FrostWyvern => (
                    Body::BirdLarge(comp::bird_large::Body::random_with(
                        &mut rng,
                        &comp::bird_large::Species::FrostWyvern,
                    )),
                    Role::Wild,
                ),
                TrackedPopulation::SeaWyvern => (
                    Body::BirdLarge(comp::bird_large::Body::random_with(
                        &mut rng,
                        &comp::bird_large::Species::SeaWyvern,
                    )),
                    Role::Wild,
                ),
                TrackedPopulation::FlameWyvern => (
                    Body::BirdLarge(comp::bird_large::Body::random_with(
                        &mut rng,
                        &comp::bird_large::Species::FlameWyvern,
                    )),
                    Role::Wild,
                ),
                TrackedPopulation::WealdWyvern => (
                    Body::BirdLarge(comp::bird_large::Body::random_with(
                        &mut rng,
                        &comp::bird_large::Species::WealdWyvern,
                    )),
                    Role::Wild,
                ),
                TrackedPopulation::Phoenix => (
                    Body::BirdLarge(comp::bird_large::Body::random_with(
                        &mut rng,
                        &comp::bird_large::Species::Phoenix,
                    )),
                    Role::Wild,
                ),
                TrackedPopulation::Roc => (
                    Body::BirdLarge(comp::bird_large::Body::random_with(
                        &mut rng,
                        &comp::bird_large::Species::Roc,
                    )),
                    Role::Wild,
                ),
                TrackedPopulation::Cockatrice => (
                    Body::BirdLarge(comp::bird_large::Body::random_with(
                        &mut rng,
                        &comp::bird_large::Species::Cockatrice,
                    )),
                    Role::Wild,
                ),
                TrackedPopulation::Other => continue 'outer,
            };

            let fake_death = Death {
                time: TimeOfDay(tod.0 - MIN_SPAWN_DELAY),
                body,
                role,
                faction: None,
            };

            data.architect.population.on_spawn(&fake_death);

            data.architect.deaths.push_front(fake_death);
        }

        count_to_spawn += count;
    }

    // The ceiling caps what this round may create; at 0 the queue is left
    // exactly as it is.
    respawn_queued(data, tod, count_to_spawn.min(allowance), |data, death| {
        spawn_npc(data, ctx.world, ctx.index, death)
    });
}

/// Works through the respawn queue, creating at most `count_to_spawn` NPCs
/// with `spawn`. Deaths that cannot spawn yet stay queued, in order.
fn respawn_queued(
    data: &mut Data,
    tod: TimeOfDay,
    mut count_to_spawn: u32,
    mut spawn: impl FnMut(&mut Data, &Death) -> bool,
) {
    // @perf: Could reuse previous allocation here.
    let mut failed_spawn = Vec::new();

    while count_to_spawn > 0
        && let Some(death) = data.architect.deaths.pop_front()
    {
        if data.architect.population.of_death(&death)
            > data.architect.wanted_population.of_death(&death)
        {
            data.architect.population.on_death(&death);
            // If we have more than enough of this npc, we skip spawning a new one.
            continue;
        }

        if death.time.0 + MIN_SPAWN_DELAY > tod.0 {
            data.architect.deaths.push_front(death);
            break;
        }

        if spawn(data, &death) {
            count_to_spawn -= 1;
        } else {
            failed_spawn.push(death);
        }
    }

    for death in failed_spawn.into_iter().rev() {
        data.architect.deaths.push_front(death);
    }
}

fn randomize_body(body: Body, rng: &mut impl RngExt) -> Body {
    let mut random_humanoid = || {
        let species = comp::humanoid::ALL_SPECIES.choose(rng).unwrap();
        Body::Humanoid(comp::humanoid::Body::random_with(rng, species))
    };
    match body {
        Body::Humanoid(_) => random_humanoid(),
        body => body,
    }
}

fn role_personality(rng: &mut impl RngExt, role: &Role) -> Personality {
    match role {
        Role::Civilised(profession) => match profession {
            Some(Profession::Guard | Profession::Merchant | Profession::Captain) => {
                Personality::random_good(rng)
            },
            Some(Profession::Cultist | Profession::Pirate(_)) => Personality::random_evil(rng),
            None
            | Some(
                Profession::Farmer
                | Profession::Chef
                | Profession::Hunter
                | Profession::Blacksmith
                | Profession::Alchemist
                | Profession::Herbalist
                | Profession::Adventurer(_),
            ) => Personality::random(rng),
        },
        Role::Wild => Personality::random(rng),
        Role::Monster => Personality::random_evil(rng),
        Role::Vehicle => Personality::default(),
    }
}

/// Whether a body lives in water (`Some(true)`), on land (`Some(false)`) or
/// does not care (`None`: flyers, objects, vehicles).
fn body_aquatic(body: &Body) -> Option<bool> {
    match body {
        Body::Crustacean(_) | Body::FishSmall(_) | Body::FishMedium(_) => Some(true),
        Body::Dragon(_)
        | Body::BirdLarge(_)
        | Body::BirdMedium(_)
        | Body::Object(_)
        | Body::Ship(_)
        | Body::Item(_)
        | Body::Plugin(_) => None,
        _ => Some(false),
    }
}

/// The spawn position is the chunk's centre column, and this checks that
/// exact column (not a chunk average). Inside an authored water region it
/// must match the body: a
/// water body in authored water, a land body on dry ground at least 2 m from
/// it (a bank lip next to a deep channel is a cliff edge, not a spawn).
/// Always true outside every region.
fn authored_spawn_ok(world: &World, wpos: Vec2<i32>, aquatic: Option<bool>) -> bool {
    match (world.sim().authored_column_at(wpos), aquatic) {
        (None, _) | (_, None) => true,
        // The natural map (an unauthored column of a partial chunk): the
        // chunk filter decides, as outside regions.
        (Some(col), _) if col.natural && col.cell == world::authored_raster::AuthoredCell::None => {
            true
        },
        (Some(col), Some(true)) => col.cell.is_wet(),
        (Some(col), Some(false)) => !col.cell.is_wet() && col.water_dist.is_none_or(|d| d >= 2.0),
    }
}

fn spawn_anywhere(
    data: &mut Data,
    world: &World,
    death: &Death,
    rng: &mut impl RngExt,
    body: Body,
    personality: Personality,
) {
    let mut attempt = |check: bool| {
        let cpos = world
            .sim()
            .map_size_lg()
            .chunks()
            .map(|s| rng.random_range(0..s as i32));

        // TODO: If we had access to `ChunkStates` here we could make sure
        // these aren't getting respawned in loaded chunks.
        let center = cpos.cpos_to_wpos_center();
        // Authored-aware: authored water and its banks count, and z comes
        // from the authored surface (see `world::authored_raster::queries`).
        if world
            .sim()
            .chunk_water(cpos)
            .is_some_and(|w| !check || !w.underwater)
            && (!check || authored_spawn_ok(world, center, body_aquatic(&body)))
        {
            let wpos = center.as_().with_z(world.sim().surface_alt_at(center));

            data.spawn_actor(
                Actor::new_npc(rng.random(), wpos, body, death.role.clone())
                    .with_personality(personality),
            );
            return true;
        }

        false
    };
    for _ in 0..RESPAWN_ATTEMPTS {
        if attempt(true) {
            return;
        }
    }
    attempt(false);
}

fn spawn_at_plot(
    data: &mut Data,
    world: &World,
    index: IndexRef,
    death: &Death,
    rng: &mut impl RngExt,
    body: Body,
    personality: Personality,
    match_plot: impl Fn(&Data, common::rtsim::SiteId, &world::site::Plot) -> bool,
) -> bool {
    let sites = &index.sites;
    let data_ref = &*data;
    let match_plot = &match_plot;
    if let Some((id, site, plot)) = data
        .sites
        .iter()
        .filter(|(_, site)| !site.is_loaded())
        .filter_map(|(id, site)| Some((id, site.world_site?)))
        .flat_map(|(id, world_site)| {
            let world_site = sites.get(world_site);
            world_site
                .filter_plots(move |plot| match_plot(data_ref, id, plot))
                .map(move |plot| (id, world_site, plot))
        })
        .choose(rng)
    {
        let wpos = site.tile_center_wpos(plot.root_tile());
        let wpos = wpos
            .as_()
            .with_z(world.sim().get_alt_approx(wpos).unwrap_or(0.0));
        let mut npc = Actor::new_npc(rng.random(), wpos, body, death.role.clone())
            .with_personality(personality)
            .with_home(id);
        if let Some(faction) = data.sites[id].faction {
            npc = npc.with_faction(faction);
        }
        data.spawn_actor(npc);

        true
    } else {
        false
    }
}

fn spawn_profession(
    data: &mut Data,
    world: &World,
    index: IndexRef,
    death: &Death,
    rng: &mut impl RngExt,
    body: Body,
    personality: Personality,
    profession: Option<Profession>,
) -> bool {
    match profession {
        Some(Profession::Pirate(captain)) => {
            spawn_at_plot(
                data,
                world,
                index,
                death,
                rng,
                body,
                personality,
                |data, s, p| {
                    // Don't spawn multiple captains at the same site.
                    if captain
                        && data.sites[s].population.iter().any(|npc| {
                            data.actors.get(*npc).is_some_and(|npc| {
                                matches!(npc.profession(), Some(Profession::Pirate(true)))
                            })
                        })
                    {
                        return false;
                    }
                    matches!(p.kind(), world::site::PlotKind::PirateHideout(_))
                },
            )
        },
        // A town NPC goes to a settlement that still has room for its
        // population (NH-171); when none has (or those that have are
        // loaded), to any house as before, so the total is never lost.
        _ => {
            spawn_at_plot(
                data,
                world,
                index,
                death,
                rng,
                body,
                personality,
                |data, s, p| p.is_house() && site_has_room(data, index, s),
            ) || spawn_at_plot(
                data,
                world,
                index,
                death,
                rng,
                body,
                personality,
                |_, _, p| p.is_house(),
            )
        },
    }
}

/// Whether a site's resident population is still below what it wants
/// ([`settlement_population`]). Sites without a settlement population keep
/// the old, unbounded behaviour.
fn site_has_room(data: &Data, index: IndexRef, site: common::rtsim::SiteId) -> bool {
    let site = &data.sites[site];
    site.world_site
        .and_then(|ws| settlement_population(index.sites.get(ws)))
        .is_none_or(|wanted| (site.population.len() as u32) < wanted.total())
}

fn spawn_npc(data: &mut Data, world: &World, index: IndexRef, death: &Death) -> bool {
    let mut rng = rng();
    let body = randomize_body(death.body, &mut rng);
    let personality = role_personality(&mut rng, &death.role);
    // First try and respawn in the same faction.
    let did_spawn = if let Some(faction_id) = death.faction
        && data.factions.get(faction_id).is_some()
    {
        if let Some((id, site)) = data
            .sites
            .iter()
            .filter(|(_, site)| site.faction == Some(faction_id) && !site.is_loaded())
            .choose(&mut rng)
        {
            let wpos = site.wpos;
            let wpos = wpos
                .as_()
                .with_z(world.sim().get_alt_approx(wpos).unwrap_or(0.0));
            data.spawn_actor(
                Actor::new_npc(rng.random(), wpos, body, death.role.clone())
                    .with_personality(personality)
                    .with_home(id)
                    .with_faction(faction_id),
            );

            true
        } else {
            false
        }
    } else {
        match &death.role {
            Role::Civilised(profession) => spawn_profession(
                data,
                world,
                index,
                death,
                &mut rng,
                body,
                personality,
                *profession,
            ),
            Role::Wild => {
                let site_filter: fn(&SiteKind) -> bool = match body {
                    Body::BirdLarge(body) => match body.species {
                        comp::bird_large::Species::Phoenix => {
                            |site| matches!(site, SiteKind::DwarvenMine)
                        },
                        comp::bird_large::Species::Cockatrice => {
                            |site| matches!(site, SiteKind::Myrmidon)
                        },
                        comp::bird_large::Species::Roc => |site| matches!(site, SiteKind::Haniwa),
                        comp::bird_large::Species::FlameWyvern => {
                            |site| matches!(site, SiteKind::Terracotta)
                        },
                        comp::bird_large::Species::CloudWyvern => {
                            |site| matches!(site, SiteKind::Sahagin)
                        },
                        comp::bird_large::Species::FrostWyvern => {
                            |site| matches!(site, SiteKind::Adlet)
                        },
                        comp::bird_large::Species::SeaWyvern => {
                            |site| matches!(site, SiteKind::ChapelSite)
                        },
                        comp::bird_large::Species::WealdWyvern => {
                            |site| matches!(site, SiteKind::GiantTree)
                        },
                    },
                    _ => |_| true,
                };

                if let Some((id, site)) = data
                    .sites
                    .iter()
                    .filter(|(_, site)| {
                        !site.is_loaded()
                            && site
                                .world_site
                                .and_then(|s| index.sites.get(s).kind)
                                .is_some_and(|s| site_filter(&s))
                    })
                    .choose(&mut rng)
                {
                    let wpos = site.wpos;
                    let wpos = wpos
                        .as_()
                        .with_z(world.sim().get_alt_approx(wpos).unwrap_or(0.0));
                    data.spawn_actor(
                        Actor::new_npc(rng.random(), wpos, body, death.role.clone())
                            .with_personality(personality)
                            .with_home(id),
                    );
                    true
                } else {
                    false
                }
            },
            Role::Monster => {
                let chunk_filter: fn(&SimChunk, &ChunkWater) -> bool = match body {
                    Body::BipedLarge(body) => match body.species {
                        comp::biped_large::Species::Tursus
                        | comp::biped_large::Species::Gigasfrost
                        | comp::biped_large::Species::Wendigo => {
                            |chunk, w| !w.underwater && chunk.temp < CONFIG.snow_temp
                        },
                        comp::biped_large::Species::Gigasfire => |chunk, w| {
                            !w.underwater
                                && chunk.temp > CONFIG.desert_temp
                                && chunk.humidity < CONFIG.desert_hum
                        },
                        comp::biped_large::Species::Mountaintroll => {
                            |chunk, w| !w.underwater && chunk.alt > 500.0
                        },
                        comp::biped_large::Species::Swamptroll => {
                            |chunk, w| !w.underwater && chunk.humidity > CONFIG.jungle_hum
                        },
                        _ => |_, w| !w.underwater,
                    },
                    Body::Arthropod(_)
                    | Body::Humanoid(_)
                    | Body::QuadrupedSmall(_)
                    | Body::BipedSmall(_)
                    | Body::QuadrupedMedium(_)
                    | Body::Golem(_)
                    | Body::Theropod(_)
                    | Body::QuadrupedLow(_) => |_, w| !w.underwater,
                    Body::Dragon(_) | Body::BirdLarge(_) | Body::BirdMedium(_) => |_, _| true,
                    Body::Crustacean(_) | Body::FishSmall(_) | Body::FishMedium(_) => {
                        |_, w| w.underwater
                    },
                    Body::Object(_) | Body::Ship(_) | Body::Item(_) | Body::Plugin(_) => {
                        |_, _| true
                    },
                };

                for _ in 0..RESPAWN_ATTEMPTS {
                    let cpos = world
                        .sim()
                        .map_size_lg()
                        .chunks()
                        .map(|s| rng.random_range(0..s as i32));

                    // TODO: If we had access to `ChunkStates` here we could make sure
                    // these aren't getting respawned in loaded chunks.
                    let center = cpos.cpos_to_wpos_center();
                    // Authored-aware, as in `spawn_anywhere`.
                    if world
                        .sim()
                        .get(cpos)
                        .zip(world.sim().chunk_water(cpos))
                        .is_some_and(|(chunk, w)| chunk_filter(chunk, &w))
                        && authored_spawn_ok(world, center, body_aquatic(&body))
                    {
                        let wpos = center.as_().with_z(world.sim().surface_alt_at(center));

                        data.spawn_actor(
                            Actor::new_npc(rng.random(), wpos, body, death.role.clone())
                                .with_personality(personality),
                        );
                        return true;
                    }
                }

                false
            },
            Role::Vehicle => {
                // Vehicles don't die as of now.
                unimplemented!()
            },
        }
    };

    // If enough time has passed, try spawning anyway.
    if !did_spawn && death.time.0 + MIN_SPAWN_DELAY * 5.0 < data.time_of_day.0 {
        match death.role {
            Role::Civilised(profession) => {
                if !spawn_profession(
                    data,
                    world,
                    index,
                    death,
                    &mut rng,
                    body,
                    personality,
                    profession,
                ) {
                    spawn_anywhere(data, world, death, &mut rng, body, personality)
                }
            },
            Role::Wild | Role::Monster => {
                spawn_anywhere(data, world, death, &mut rng, body, personality)
            },
            Role::Vehicle => {
                // Vehicles don't die as of now.
                unimplemented!()
            },
        }

        true
    } else {
        did_spawn
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        data::{CURRENT_VERSION, Nature, nature},
        generate::SettlementPopulation,
    };
    use common::{grid::Grid, rtsim::Profession};
    use vek::Vec3;

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

    #[test]
    fn settlement_population_splits_keep_today_and_hit_authored_totals() {
        // Today's rule, unchanged.
        let p = SettlementPopulation::from_plot_count(347);
        assert_eq!(
            (p.guards, p.adventurers, p.merchants, p.others),
            (86, 69, 58, 192)
        );
        assert_eq!(SettlementPopulation::from_plot_count(0).total(), 1);
        // An authored total is exact, in roughly the same proportions.
        for total in [0, 1, 7, 10, 50, 350, 400, 5_000] {
            assert_eq!(SettlementPopulation::from_total(total).total(), total);
        }
        let k = SettlementPopulation::from_total(350);
        assert_eq!(
            (k.guards, k.adventurers, k.merchants, k.others),
            (75, 60, 50, 165)
        );
    }
}
