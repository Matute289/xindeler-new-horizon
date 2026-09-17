use vek::Vec2;
use xindeler_world::{
    CONFIG,
    sim::{self, WorldOpts, WorldSim},
    site::SiteKind,
};

fn main() {
    let threadpool = rayon::ThreadPoolBuilder::new().build().unwrap();
    let sim = WorldSim::generate(
        0,
        WorldOpts {
            seed_elements: true,
            world_file: sim::FileOpts::LoadAsset("world.map.cromatolis_v0".to_string()),
            calendar: None,
        },
        &threadpool,
        &|_| {},
    );
    for y in 758..=778 {
        for x in 820..=844 {
            let loc = Vec2::new(x, y);
            let Some(chunk) = sim.get(loc) else { continue };
            if SiteKind::Myrmidon.is_suitable_loc(loc, &sim)
                || (chunk.water_alt - CONFIG.sea_level) > 50.0
            {
                println!(
                    "{x:3},{y:3} suitable={} alt={:7.2} water={:7.2} relief={:6.2} temp={:.3} grad={:.3?} land={} river={} cliffs={}",
                    SiteKind::Myrmidon.is_suitable_loc(loc, &sim),
                    chunk.alt,
                    chunk.water_alt,
                    chunk.water_alt - CONFIG.sea_level,
                    chunk.temp,
                    sim.get_gradient_approx(loc),
                    !matches!(chunk.get_biome(), common::terrain::BiomeKind::Lake | common::terrain::BiomeKind::Ocean),
                    chunk.river.near_water(),
                    chunk.near_cliffs(),
                );
            }
        }
    }
}
