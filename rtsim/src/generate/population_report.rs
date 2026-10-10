//! NH-171 stage-1 measurement probe (ignored; needs the Cromatolis LFS
//! assets).

use super::*;
use crate::data::architect::TrackedPopulation as T;
use std::time::Instant;
use vek::Vec3;

/// NH-171 stage-1 measurement probe on the real Cromatolis world (needs
/// the LFS assets): the wanted population by role and by settlement,
/// `Data::generate`'s time, and the save size and write time of the
/// rtsim data with 3k / 6k / 12k simulated NPCs. Prints, asserts only
/// sanity. `cargo test -p xindeler-rtsim --release
/// cromatolis_population_report -- --ignored --nocapture`
#[test]
#[ignore]
fn cromatolis_population_report() {
    let threadpool = rayon::ThreadPoolBuilder::new().build().unwrap();
    let (world, index) = World::generate(
        0,
        world::sim::WorldOpts {
            seed_elements: true,
            world_file: world::sim::FileOpts::LoadAsset("world.map.cromatolis_v0".to_string()),
            calendar: None,
        },
        &threadpool,
        &|_| {},
    );
    let index = index.as_index_ref();

    let started = Instant::now();
    let data = Data::generate(&WorldSettings::default(), &world, index);
    let generate_ms = started.elapsed().as_secs_f64() * 1e3;

    let wanted = &data.architect.wanted_population;
    let groups = wanted.groups();
    let others = wanted.get(T::OtherTownNpcs);
    println!(
        "== wanted population (world seed 0): total {}",
        wanted.total()
    );
    println!("Data::generate: {generate_ms:.1} ms");
    println!(
        "groups: civilians {} | pirates {} | cultists {} | monsters {} | wild {} | other {}",
        groups.civilians,
        groups.pirates,
        groups.cultists,
        groups.monsters,
        groups.wild,
        groups.other
    );
    for (pop, n) in wanted.iter() {
        println!("  {pop:?}: {n}");
    }
    // `OtherTownNpcs` draws its profession at spawn: 4/10 farmer,
    // 2/10 herbalist, 1/10 each hunter, blacksmith, chef, alchemist.
    println!(
        "  OtherTownNpcs expected: farmers {:.0}, herbalists {:.0}, \
         hunters/blacksmiths/chefs/alchemists {:.0} each",
        others as f64 * 0.4,
        others as f64 * 0.2,
        others as f64 * 0.1
    );
    let airship_captains = data
        .actors
        .values()
        .filter(|a| matches!(a.role, Role::Civilised(Some(Profession::Captain))))
        .count();
    println!(
        "  plus {airship_captains} airship captains created at generation (not architect \
         population)"
    );

    let mut settlements: Vec<_> = world
        .civs()
        .sites
        .iter()
        .filter_map(|(_, civ_site)| {
            let site = index.sites.get(civ_site.site_tmp?);
            let pop = settlement_population(site)?;
            Some((
                civ_site.authored_id().unwrap_or("(procedural)").to_string(),
                site.plots().len(),
                site.npc_count,
                pop,
            ))
        })
        .collect();
    settlements.sort_by_key(|(id, _, _, pop)| (std::cmp::Reverse(pop.total()), id.clone()));
    println!("== settlements: {}", settlements.len());
    println!("id\tplots\tnpc_count\ttotal\tguards\tadventurers\tmerchants\tothers");
    for (id, plots, npc_count, pop) in &settlements {
        println!(
            "{id}\t{plots}\t{npc_count:?}\t{}\t{}\t{}\t{}\t{}",
            pop.total(),
            pop.guards,
            pop.adventurers,
            pop.merchants,
            pop.others
        );
    }
    let town_total: u32 = settlements.iter().map(|(.., pop)| pop.total()).sum();
    assert_eq!(
        town_total,
        wanted.get(T::Guards)
            + wanted.get(T::Adventurers)
            + wanted.get(T::Merchants)
            + wanted.get(T::OtherTownNpcs)
    );

    println!("== simulated NPC counts (rtsim save = MessagePack of Data)");
    println!("npcs\tspawn_ms\tsave_bytes\tsave_ms\tload_ms\tactor_struct_bytes");
    for n in [0u32, 3_000, 6_000, 12_000] {
        let mut data = data.clone();
        let started = Instant::now();
        for i in 0..n {
            let body = comp::Body::Humanoid(comp::humanoid::Body::random());
            data.spawn_actor(
                Actor::new_npc(
                    i,
                    Vec3::new(1000.0, 1000.0, 100.0),
                    body,
                    Role::Civilised(Some(Profession::Farmer)),
                )
                .with_personality(Personality::default()),
            );
        }
        let spawn_ms = started.elapsed().as_secs_f64() * 1e3;
        let started = Instant::now();
        let mut bytes = Vec::new();
        data.write_to(&mut bytes).unwrap();
        let save_ms = started.elapsed().as_secs_f64() * 1e3;
        let started = Instant::now();
        let Ok(loaded) = Data::from_reader(&bytes[..]) else {
            panic!("save must load");
        };
        let load_ms = started.elapsed().as_secs_f64() * 1e3;
        assert_eq!(loaded.actors.len(), data.actors.len());
        println!(
            "{n}\t{spawn_ms:.1}\t{}\t{save_ms:.1}\t{load_ms:.1}\t{}",
            bytes.len(),
            std::mem::size_of::<Actor>()
        );
    }
}
