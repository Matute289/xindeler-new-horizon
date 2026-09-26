//! Full-world tests for the authored Cromatolis region.
//!
//! These live in their own file rather than in a `mod tests` appended to
//! `lib.rs` for one reason: `lib.rs` is upstream-owned and upstream's
//! `master` keeps editing it, while upstream has no test module there at
//! all. A fork-only block bolted onto upstream's last line is the noisiest
//! possible shape for the monthly merge; one `mod` line is not. Same
//! reasoning that moved `cave.rs`'s measurement harnesses to that file's
//! tail in COW-23 Phase 6.
//!
//! Everything here needs the real Cromatolis LFS assets pulled locally
//! (`git lfs pull` against the VPS store) and is therefore `#[ignore]`d.

use super::*;
use crate::site::{PlotKind, PortClass, Structure};

/// Requires the real Cromatolis LFS assets to be pulled locally (`git lfs
/// pull` against the VPS store); not run automated, matching this
/// crate's existing precedent for tests whose meaningful assertion
/// depends on real, environment-specific data. Recommended command:
/// `cargo test -p xindeler-world
/// ineligible_authored_settlements_never_appear_as_possible_starting_sites
/// -- --ignored`
#[test]
#[ignore]
fn ineligible_authored_settlements_never_appear_as_possible_starting_sites() {
    let threadpool = rayon::ThreadPoolBuilder::new().build().unwrap();
    let (mut world, index) = World::generate(
        0,
        sim::WorldOpts {
            seed_elements: true,
            world_file: sim::FileOpts::LoadAsset("world.map.cromatolis_v0".to_string()),
            calendar: None,
        },
        &threadpool,
        &|_| {},
    );
    let index_ref = index.as_index_ref();

    // No settlement in the real export is currently marked
    // `start_eligible: false`, so this test exercises the exclusion
    // mechanism directly: take the *baseline* `possible_starting_sites`
    // result, pick a real authored settlement that's actually part of
    // it (not just any authored settlement -- most of the 65 wouldn't
    // rank in the top slots anyway, so excluding an arbitrary one
    // wouldn't move the result), force-exclude it, and confirm it drops
    // out.
    let baseline = world.get_map_data(index_ref, &threadpool);
    let site_tmp_to_civ_site_id: std::collections::HashMap<_, _> = world
        .civs
        .sites
        .iter()
        .filter_map(|(civ_site_id, site)| Some((site.site_tmp?.id(), civ_site_id)))
        .collect();
    let target_site_id = *baseline
        .possible_starting_sites
        .iter()
        .find(|site_tmp| {
            site_tmp_to_civ_site_id
                .get(site_tmp)
                .is_some_and(|&civ_site_id| {
                    world
                        .civs
                        .sites
                        .get(civ_site_id)
                        .is_authored_starting_settlement()
                })
        })
        .expect(
            "at least one real authored settlement must rank as a possible starting site for this \
             test to be meaningful",
        );
    let target_civ_site_id = site_tmp_to_civ_site_id[&target_site_id];

    world
        .civs
        .sites
        .get_mut(target_civ_site_id)
        .set_start_eligible_for_test(false);

    let after_exclusion = world.get_map_data(index_ref, &threadpool);
    assert!(
        !after_exclusion
            .possible_starting_sites
            .contains(&target_site_id),
        "an authored settlement explicitly marked start_eligible: false was still returned as a \
         possible starting site"
    );
}

/// **COW-23 T20 — the post-flip measurement pass.**
///
/// `cromatolis_v0_procedural_layers.ron` shipped `caves: false, rocks:
/// false` from COW-15 until COW-23's T15 flipped both. Everything Phases
/// 1–3 built exists to make that flip safe, and every number measured
/// before it was measured through a *probe* — a read-only replica of a
/// generation path, or a layer forced on for one chunk at a time. This is
/// the one measurement taken through the real thing: whole chunks, out of
/// `World::generate_chunk`, in the configuration the region actually
/// ships.
///
/// Each sampled chunk is generated three times — layers off, `caves` only,
/// and the shipped `caves + rocks` — and the three are compared voxel for
/// voxel underground. The split into three matters, because the two
/// layers sit on opposite sides of the authored carve in
/// `World::generate_chunk`:
///
/// * `apply_caves_to` runs **before** `apply_cromatolis_*`, so the authored
///   carve always gets the last word over it. Not one authored void voxel may
///   be lost to the procedural cave layer, and that is the assertion below
///   rather than a printed number.
///
///   **What it proves, precisely: the ordering, end to end.** It is not a
///   test of the `AuthoredVoids` guard — a `Seal` failure that let a
///   tunnel breach a sealed cave from *outside* the authored footprint
///   would be invisible here, because the authored re-carve never touches
///   those voxels. The guard's own proof is the
///   sealed-caves-breached-after-the-guard measurement in
///   `layer::cromatolis_cave_features` (6 → 0). What had never been
///   measured before this test is that the ordering holds on real
///   generated chunks, rather than on a reading of `generate_chunk`.
/// * `apply_rocks_to` runs **after** it, so a boulder legitimately *can* occupy
///   authored void. That is the point of axis B: the guarantee is a walkable
///   bypass (`rock_traversal`, V6*), not an untouched volume. So the rocks side
///   is characterised and printed, never asserted to be zero — asserting it
///   would be asserting the feature away.
///
/// The sample is the authored-void chunks themselves, strided across the
/// whole set: that is where the two layers can actually interact, so a
/// uniform sample of the region would spend most of its budget proving
/// nothing. The stride is over
/// [`AuthoredVoids::occupied_chunks`](crate::layer::authored_voids), which
/// sorts before returning, so the sample is the same on every run.
///
/// **Do not expect bit-identical chunks across runs, and do not read any
/// count here as exact.** `World::generate_chunk` seeds its `dynamic_rng`
/// from entropy (`ChaCha8Rng::from_seed(rand::rng().random())`), so two
/// generations of the same chunk are not the same chunk. Restricting every
/// count to *carved geometry* (`is_filled()`) removes nearly all of that —
/// `apply_rocks_to`, `apply_shrubs_to`, `apply_scatter_to` and
/// `apply_terrain_damage_to` all ignore their `dynamic_rng` argument
/// outright, and `apply_trees_to`'s two uses of it place a `Lantern` and
/// hanging sprites, which are air blocks carrying a sprite rather than
/// filled ones — but not all of it: `write_column`'s structure and loot
/// draws in `layer::cave` do take that RNG and can write solid blocks.
/// (`apply_spots_to` does not, despite being handed it: it seeds its own
/// `ChaChaRng` per spot. So the residual variance lives entirely on the
/// `caves`-on side.)
/// So the test measures its own **noise floor** (the same chunks generated
/// twice under one policy) and prints it alongside, and nothing below that
/// floor is worth a conclusion. That is why the one hard assertion here
/// re-tests each candidate against a second generation before counting it,
/// and why the rest are printed rather than asserted.
///
/// `cargo test -p xindeler-world --release \
/// cromatolis_procedural_layer_flip -- --ignored --nocapture`
#[test]
#[ignore]
fn cromatolis_procedural_layer_flip_adds_volume_without_costing_authored_void() {
    use crate::{layer::authored_regions::authored_voids_for, sim::AuthoredProceduralLayers};

    /// How many authored-void chunks to regenerate, three times each.
    /// Matches the sample size `T12`/V8 used for the guard's cost, so the
    /// two are talking about the same slice of the region.
    const T20_CHUNK_SAMPLE: usize = 2_000;
    /// How far above a column's surface to look for boulder volume. The
    /// tallest rock kind the lattice places is far below this.
    const ABOVE_SURFACE_BAND: i32 = 64;
    /// How many of the sampled chunks to generate a second time under the
    /// shipped policy, to measure the run-to-run variance every other
    /// count has to be read against. Enough for an order of magnitude;
    /// this is a floor to compare against, not a figure to publish.
    const NOISE_FLOOR_CHUNKS: u64 = 64;
    /// Cap on the candidate losses kept per chunk. A working guard finds
    /// none; a broken one would find millions, and the failure message
    /// needs a handful rather than a heap full.
    const LOSS_EXAMPLES_PER_CHUNK: usize = 64;

    let threadpool = rayon::ThreadPoolBuilder::new().build().unwrap();
    let (mut world, index) = World::generate(
        0,
        sim::WorldOpts {
            seed_elements: true,
            world_file: sim::FileOpts::LoadAsset("world.map.cromatolis_v0".to_string()),
            calendar: None,
        },
        &threadpool,
        &|_| {},
    );
    let index_ref = index.as_index_ref();

    let shipped = world
        .sim()
        .authored_procedural_layers()
        .expect("the authored region must declare its procedural layer policy");
    assert!(
        shipped.caves && shipped.rocks,
        "this test measures the effect of COW-23 T15's flip. If \
         `cromatolis_v0_procedural_layers.ron` has gone back to `caves: false` / `rocks: false`, \
         this assertion is the tripwire that catches it -- every count below would otherwise \
         collapse to a silent zero for an unrelated reason"
    );
    assert!(
        index_ref.features.caves && index_ref.features.rocks,
        "`assets/world/features.ron` switches one of these off globally, so the region policy \
         composes to off and this measurement cannot run"
    );

    let layers_off = AuthoredProceduralLayers {
        caves: false,
        rocks: false,
        ..shipped
    };
    let layers_caves_only = AuthoredProceduralLayers {
        rocks: false,
        ..shipped
    };

    // ---- (d) MarkerKind::Cave, through the real map-data path ----
    //
    // Not `surface_entrances().count()`: that is the *input* to the
    // marker list, and the gate that was the whole point of the flip
    // (`authored_procedural_caves_enabled`) sits downstream of it, in
    // `get_map_data`. Counting what a client would actually receive is
    // the only version of this number that proves the flip reached the
    // map.
    let cave_markers = |world: &World| {
        world
            .get_map_data(index_ref, &threadpool)
            .sites
            .iter()
            .filter(|marker| matches!(marker.kind, MarkerKind::Cave))
            .count()
    };
    let markers_after = cave_markers(&world);
    world.set_authored_procedural_layers_for_test(layers_off);
    let markers_before = cave_markers(&world);
    world.set_authored_procedural_layers_for_test(shipped);

    // Asserted here, not at the end: everything it depends on is already
    // known, and the sweep below is minutes long. Failing a long
    // `--release --ignored` run at the finish line on something decided in
    // its first seconds is a bad way to spend someone's afternoon.
    //
    // `0` is the structural invariant -- the gate in `get_map_data` is a
    // plain `&&` on the region policy, so a world that skips
    // `apply_caves_to` must publish no cave marker at all. The count on the
    // other side is content, and it is pinned exactly once, by
    // `the_real_region_still_derives_its_surface_cave_markers`. So this
    // asserts the map publishes *everything the derivation produced* rather
    // than restating the number and becoming a second place a catalogue
    // edit has to be applied.
    let derived = layer::cave::surface_entrances(&Land::from_sim(world.sim()), index_ref).count();
    assert_eq!(
        (markers_before, markers_after),
        (0, derived),
        "the cave-marker gate moved: {markers_before} before the flip and {markers_after} after, \
         against {derived} surface entrances derived from the lattice"
    );

    // ---- the chunk sample ----
    let sample: Vec<Vec2<i32>> =
        CanvasInfo::with_mock_canvas_info(index_ref, world.sim(), |info| {
            let voids = authored_voids_for(info)
                .expect("the authored Cromatolis region must index its voids");
            let chunks: Vec<Vec2<i32>> = voids
                .occupied_chunks()
                .into_iter()
                // Edge chunks have no `base_z` and generate as the
                // out-of-bounds stub, which has no layers applied to it at
                // all and would read as "nothing changed".
                .filter(|cpos| info.chunks().get_base_z(*cpos).is_some())
                .collect();
            // Floor, not `div_ceil`: a stride that rounds up yields fewer
            // than `T20_CHUNK_SAMPLE` chunks and the sample silently
            // shrinks. Rounding down over-yields and `take` trims, so the
            // count is exactly the one this measurement claims.
            let stride = (chunks.len() / T20_CHUNK_SAMPLE).max(1);
            chunks
                .into_iter()
                .step_by(stride)
                .take(T20_CHUNK_SAMPLE)
                .collect()
        });
    // A floor rather than an equality: the sample's size is bounded by
    // how many void chunks the catalogue produces, which is content and
    // may legitimately shrink. The real count is printed with the results,
    // so a shrunken sample is visible rather than silent.
    assert!(
        sample.len() >= T20_CHUNK_SAMPLE / 2,
        "only {} authored-void chunks are generatable, far short of the {T20_CHUNK_SAMPLE} this \
         measurement is sized for -- the void index or the map changed shape",
        sample.len(),
    );

    // ---- (c) boulders, from the lattice that decides them ----
    //
    // `rock_at` is the one place the RNG cascade that picks a rock lives,
    // and `apply_rocks_to` calls exactly it, over the same `rock_lattice`,
    // so this counts the candidates generation *decides on*.
    //
    // It is deliberately NOT called the stamped count, and nothing is
    // asserted on it. Two reasons. It is an upper bound: this region ships
    // `rock_traversal_repair: true`, and `accommodate` may refuse a rock
    // outright (on this map it refuses none -- `repaired: 0, rejected: 0`
    // over all 1 863 225 candidates, per
    // `real_world_rock_traversal_measurement` -- so the bound is tight
    // today, but it is a bound). And it is a *replica*: it never consults
    // `AuthoredProceduralLayers` and never calls `generate_chunk`, so it
    // returns the same number with `rocks: false` and cannot witness the
    // flip at all. `boulder_voxels_above_surface`, which is a real diff of
    // two generated chunks, is what carries that claim below.
    let boulders_decided = CanvasInfo::with_mock_canvas_info(index_ref, world.sim(), |info| {
        let lattice = crate::layer::rock::rock_lattice(info.index().seed);
        let mut found = 0_usize;
        for cpos in &sample {
            let min = cpos.cpos_to_wpos();
            let max = min + TerrainChunkSize::RECT_SIZE.map(|e| e as i32);
            for (wpos, seed) in lattice.iter(min, max) {
                // `iter` yields the lattice cells overlapping the rect,
                // whose jittered origins can land outside it; counting
                // those would double-count across neighbouring chunks.
                if wpos.x < min.x || wpos.y < min.y || wpos.x >= max.x || wpos.y >= max.y {
                    continue;
                }
                if info
                    .col_or_gen(wpos)
                    .and_then(|col| crate::layer::rock::rock_at(wpos, seed, col.as_ref()))
                    .is_some()
                {
                    found += 1;
                }
            }
        }
        found
    });

    // ---- (a) + (b) + (e), voxel by voxel ----
    let generate = |world: &World, cpos: Vec2<i32>| {
        world
            .generate_chunk(index_ref, cpos, None, || false, None, None)
            .map(|(chunk, _)| chunk)
            .expect("generating an in-bounds authored chunk must succeed")
    };
    let open = |chunk: &TerrainChunk, pos: Vec3<i32>| {
        // `Chonk::get` takes chunk-relative x/y with *absolute* world z,
        // and answers for positions outside the stored band too (stone
        // below, air above), which is what makes one shared z band usable
        // across three chunks whose carves gave them different depths.
        chunk.get(pos).is_ok_and(|block| !block.is_filled())
    };

    let started = std::time::Instant::now();
    // Named for what it is: every non-solid voxel below the column's topmost
    // solid block with both layers off. That is authored void plus whatever
    // else is down there with the procedural layers off -- water, a site's
    // own interior volume. A superset, which only strengthens the
    // zero-loss claim below (more voxels required to survive, not fewer),
    // but it is not a count of authored cave air and must not be quoted as
    // one.
    let mut open_before = 0_u64;
    let mut caves_open = 0_u64;
    let mut full_open = 0_u64;
    let mut tunnel_voxels = 0_u64;
    let mut authored_void_lost_to_caves = 0_u64;
    let mut loss_candidates = 0_u64;
    let mut lost_examples: Vec<Vec3<i32>> = Vec::new();
    let mut authored_void_lost_to_rocks = 0_u64;
    let mut rock_opened_voxels = 0_u64;
    let mut boulder_voxels_above_surface = 0_u64;
    // The production cost of the flip, which is the number a server operator
    // needs and which nothing else in this row measures: wall time per
    // `generate_chunk`, and the chunk's own depth. The second matters as much
    // as the first -- `Chonk::set` below `z_offset` *prepends* subchunks, and
    // `caves: true` is what drops a chunk's floor from `base_z` to several
    // hundred blocks down, which is server RAM per loaded chunk and bytes on
    // the wire to every client.
    let mut gen_time = [std::time::Duration::ZERO; 3];
    let mut sub_chunks = [0_u64; 3];
    let mut noise_chunks = 0_u64;
    let mut noise_voxels = 0_u64;
    let mut noise_band = 0_u64;

    for &cpos in &sample {
        let mut timed = |world: &World, slot: usize| {
            let at = std::time::Instant::now();
            let chunk = generate(world, cpos);
            gen_time[slot] += at.elapsed();
            sub_chunks[slot] += chunk.sub_chunks_len() as u64;
            chunk
        };
        world.set_authored_procedural_layers_for_test(layers_off);
        let off = timed(&world, 0);
        world.set_authored_procedural_layers_for_test(layers_caves_only);
        let caves = timed(&world, 1);
        world.set_authored_procedural_layers_for_test(shipped);
        let full = timed(&world, 2);

        // The noise floor, on the first few chunks only: the shipped
        // policy generated a *second* time, so the run-to-run variance in
        // carved geometry is a measured number rather than an assumption.
        // It is not zero -- `write_column`'s structure and loot draws in
        // `layer::cave` take the entropy-seeded `dynamic_rng` and can
        // write filled blocks -- so every count below has to be read
        // against it.
        let noise = (noise_chunks < NOISE_FLOOR_CHUNKS).then(|| {
            noise_chunks += 1;
            generate(&world, cpos)
        });

        let floor = off.get_min_z().min(caves.get_min_z()).min(full.get_min_z());
        let mut chunk_losses: Vec<Vec3<i32>> = Vec::new();
        for y in 0..TerrainChunkSize::RECT_SIZE.y as i32 {
            for x in 0..TerrainChunkSize::RECT_SIZE.x as i32 {
                // The surface, taken from the *unmodified* chunk, so all
                // three are measured against the same boundary: with the
                // layers on, a tunnel mouth or a boulder can move a
                // column's own highest solid block.
                let mut surface = off.get_max_z();
                while surface > floor && open(&off, Vec3::new(x, y, surface - 1)) {
                    surface -= 1;
                }

                for z in floor..surface {
                    let pos = Vec3::new(x, y, z);
                    let (o, c, f) = (open(&off, pos), open(&caves, pos), open(&full, pos));
                    open_before += u64::from(o);
                    caves_open += u64::from(c);
                    full_open += u64::from(f);
                    match (o, c) {
                        (false, true) => tunnel_voxels += 1,
                        // The count is uncapped; only the position list is
                        // capped, because a genuinely broken guard would
                        // find millions and the failure message needs a
                        // handful of them, not a heap full. The re-test
                        // below therefore confirms a *sample* of the
                        // candidates, which is why the assertion says "at
                        // least".
                        (true, false) => {
                            loss_candidates += 1;
                            if chunk_losses.len() < LOSS_EXAMPLES_PER_CHUNK {
                                chunk_losses.push(pos);
                            }
                        },
                        _ => {},
                    }
                    if o && !f {
                        authored_void_lost_to_rocks += 1;
                    }
                    if !c && f {
                        rock_opened_voxels += 1;
                    }
                    if let Some(noise) = &noise {
                        noise_band += 1;
                        noise_voxels += u64::from(open(noise, pos) != f);
                    }
                }

                // `caves` vs `full`, not `off` vs `full`: with the cave
                // layer on, tree and shrub tunnel-avoidance also changes
                // above-ground geometry, and attributing that to boulders
                // would overstate them.
                //
                // The noise comparison runs over this band too. It is the
                // more entropy-exposed of the two: the layers that consume
                // `dynamic_rng` write at and above the surface, not deep
                // underground, so a floor measured only below ground would
                // not be a floor for this number.
                for z in surface..surface + ABOVE_SURFACE_BAND {
                    let pos = Vec3::new(x, y, z);
                    let f = open(&full, pos);
                    if open(&caves, pos) && !f {
                        boulder_voxels_above_surface += 1;
                    }
                    if let Some(noise) = &noise {
                        noise_band += 1;
                        noise_voxels += u64::from(open(noise, pos) != f);
                    }
                }
            }
        }

        // A candidate loss is re-tested against a second `caves` chunk
        // before it counts, so the one hard assertion in this test cannot
        // be tripped by the noise floor above. Paid for only in the chunks
        // that produce a candidate at all -- normally none.
        if !chunk_losses.is_empty() {
            world.set_authored_procedural_layers_for_test(layers_caves_only);
            let again = generate(&world, cpos);
            world.set_authored_procedural_layers_for_test(shipped);
            for pos in chunk_losses {
                if !open(&again, pos) {
                    authored_void_lost_to_caves += 1;
                    if lost_examples.len() < 16 {
                        lost_examples.push(cpos.cpos_to_wpos().with_z(0) + pos);
                    }
                }
            }
        }
    }

    println!(
        "COW-23 T20 -- the flip, measured on {} authored-void chunks ({:.1} s)\n  (a) \
         procedural tunnel voxels carved:        {tunnel_voxels}\n  (b) authored void voxels, \
         layers off:            {open_before}\n      ... still open with `caves` on:        \
         {}\n      ... still open with `caves` + `rocks` on: {}\n      lost to `caves`:       \
         {}   lost to `rocks`: {authored_void_lost_to_rocks}\n  (c) boulders the lattice \
         decides here:      0 -> {boulders_decided}\n      boulder voxels above the surface:    \
         {boulder_voxels_above_surface}\n      solid voxels the `rocks` pass leaves open: \
         {rock_opened_voxels}\n  (d) MarkerKind::Cave on the world map:   {markers_before} -> \
         {markers_after}\n  underground open voxels, off / caves / full: {open_before} / \
         {caves_open} / {full_open}\n  noise floor: {noise_voxels} voxel(s) differ between \
         two generations of the same {noise_chunks} chunks under one policy, over \
         {noise_band} compared ({loss_candidates} loss candidate(s) seen before re-testing)\n  \
         production cost of the flip, per chunk:\n    generate_chunk:    {:.2} ms off -> {:.2} \
         ms caves -> {:.2} ms caves+rocks\n    stored sub-chunks: {:.1} off -> {:.1} caves -> \
         {:.1} caves+rocks   (server RAM per loaded chunk, and bytes on the wire per client)",
        sample.len(),
        started.elapsed().as_secs_f64(),
        open_before - authored_void_lost_to_caves,
        open_before - authored_void_lost_to_rocks,
        authored_void_lost_to_caves,
        gen_time[0].as_secs_f64() * 1e3 / sample.len() as f64,
        gen_time[1].as_secs_f64() * 1e3 / sample.len() as f64,
        gen_time[2].as_secs_f64() * 1e3 / sample.len() as f64,
        sub_chunks[0] as f64 / sample.len() as f64,
        sub_chunks[1] as f64 / sample.len() as f64,
        sub_chunks[2] as f64 / sample.len() as f64,
    );
    for pos in &lost_examples {
        println!("  lost to `caves` at {pos:?}");
    }

    // (e) The zero-collision assertion, and the one claim of this row that
    // is a hard invariant rather than a characterisation.
    assert_eq!(
        authored_void_lost_to_caves, 0,
        "the procedural cave layer took at least {authored_void_lost_to_caves} voxel(s) of \
         authored void away, reproducibly (of {loss_candidates} candidate(s); only a capped \
         sample per chunk is re-tested). It runs before the authored carve, so this cannot happen \
         by ordering alone -- something now writes authored volume after \
         `apply_cromatolis_cave_features_to`, or the authored carve stopped being total over its \
         own footprint. The first few are printed above."
    );
    assert!(
        open_before > 0,
        "the sample found no authored void at all, so the assertion above passed vacuously"
    );
    assert!(
        tunnel_voxels > 0,
        "`caves: true` carved nothing anywhere in the sample, so the flip did not reach \
         `apply_caves_to`"
    );
    assert!(
        boulder_voxels_above_surface > 0,
        "generating with `rocks: true` produced no solid voxel that generating with it off did \
         not, so the flip did not reach `apply_rocks_to`. This is the end-to-end check: \
         `boulders_decided` below is a replica of the lattice and would report the same count \
         either way."
    );
}

/// What hoisting the authored-void bucket out of the column loop saves
/// `apply_caves_to`, and what localising the authored-passage lookup saves
/// `apply_rocks_to`.
///
/// Both were recorded as production-perf follow-ups when COW-23's `caves` and
/// `rocks` toggles were flipped on: the carve resolved a chunk-granular bucket
/// once per *column*, and the rock pass answered a world-constant question
/// with a scan over every indexed shape once per *rock candidate*. Neither was
/// a correctness problem, and neither was worth a ride-along on a content
/// flip -- so each gets its own measurement here, run against the real region
/// rather than reasoned about.
///
/// Reported, not asserted to a threshold, for the same reason every other
/// figure in this file is: the numbers move with the machine and with the
/// catalogue. The assertions are the *shape* claims -- the hoisted form is
/// cheaper, and the localised lookup answers the same question of fewer
/// shapes -- which are properties rather than readings.
///
/// Requires the real Cromatolis LFS assets. Recommended command:
/// `cargo test --release -p xindeler-world
/// authored_void_lookup_costs_on_the_live_carve_and_rock_paths -- --ignored
/// --nocapture`
#[test]
#[ignore]
fn authored_void_lookup_costs_on_the_live_carve_and_rock_paths() {
    use crate::{
        layer::{
            authored_regions::authored_voids_for,
            authored_voids::chunk_voids_at,
            rock,
            traversal::{AccommodationTier, TraversalParams, analysis_footprint},
        },
        util::RandomField,
    };
    use std::time::Instant;

    /// Chunks of the authored footprint to sweep for both measurements.
    const CHUNK_SAMPLE: usize = 2_000;
    /// How many times to repeat the timed rock loop. One pass over the rocks
    /// a 2 000-chunk sample places is a few hundred lookups, which is below
    /// the clock's useful resolution; the *rate* is what is being measured.
    const ROCK_REPEATS: u32 = 200;

    let threadpool = rayon::ThreadPoolBuilder::new().build().unwrap();
    let (world, index) = World::generate(
        0,
        sim::WorldOpts {
            seed_elements: true,
            world_file: sim::FileOpts::LoadAsset("world.map.cromatolis_v0".to_string()),
            calendar: None,
        },
        &threadpool,
        &|_| {},
    );
    let index_ref = index.as_index_ref();

    CanvasInfo::with_mock_canvas_info(index_ref, world.sim(), |info| {
        let voids =
            authored_voids_for(info).expect("the authored Cromatolis region must index its voids");
        let occupied = voids.occupied_chunks();
        let stride = (occupied.len() / CHUNK_SAMPLE).max(1);
        let chunks: Vec<Vec2<i32>> = occupied
            .into_iter()
            .step_by(stride)
            .take(CHUNK_SAMPLE)
            .collect();
        let chunk_size = TerrainChunkSize::RECT_SIZE.map(|e| e as i32);
        let columns_per_chunk = (chunk_size.x * chunk_size.y) as u64;

        // ---- (1) the carve guard: one bucket lookup per chunk, not per column
        //
        // The "before" side goes through `authored_voids_for(info)` per column
        // as well, because the pre-change `tunnel_bounds_at_from` did: it is a
        // warm `OnceLock` read, not free, and leaving it out would understate
        // the saving.
        let mut sink = 0_u64;
        let start = Instant::now();
        for cpos in &chunks {
            let chunk_wpos = cpos.cpos_to_wpos();
            for y in 0..chunk_size.y {
                for x in 0..chunk_size.x {
                    let wpos2d = chunk_wpos + Vec2::new(x, y);
                    sink += u64::from(chunk_voids_at(authored_voids_for(info), wpos2d).is_some());
                }
            }
        }
        let per_column = start.elapsed();

        // The "after" side's inner loop is constant-folded away, which is
        // exactly the point: what remains is the one resolve per chunk.
        let start = Instant::now();
        for cpos in &chunks {
            let chunk_wpos = cpos.cpos_to_wpos();
            let resolved = chunk_voids_at(authored_voids_for(info), chunk_wpos);
            for _ in 0..columns_per_chunk {
                sink += u64::from(resolved.is_some());
            }
        }
        let per_chunk = start.elapsed();

        // Every column of a chunk must agree with the chunk's own answer, or
        // hoisting the lookup would change what the guard does -- the one way
        // this "pure speedup" could fail to be one.
        for cpos in chunks.iter().take(64) {
            let chunk_wpos = cpos.cpos_to_wpos();
            let resolved = chunk_voids_at(Some(voids), chunk_wpos).map(|v| v.is_empty());
            for y in 0..chunk_size.y {
                for x in 0..chunk_size.x {
                    let wpos2d = chunk_wpos + Vec2::new(x, y);
                    assert_eq!(
                        chunk_voids_at(Some(voids), wpos2d).map(|v| v.is_empty()),
                        resolved,
                        "column {wpos2d:?} resolves a different bucket from its own chunk at \
                         {cpos:?}; the grid is no longer chunk-granular and the hoist is unsound"
                    );
                }
            }
        }

        let total_columns = chunks.len() as u64 * columns_per_chunk;
        println!(
            "cave carve guard, {} chunks x {columns_per_chunk} columns:\n    bucket resolved per \
             column: {per_column:?} ({:.1} ns/column, {:.1} us/chunk)\n    bucket resolved per \
             chunk:  {per_chunk:?}\n    saving: {:.1} us per chunk [sink {sink}]",
            chunks.len(),
            per_column.as_secs_f64() * 1e9 / total_columns as f64,
            per_column.as_secs_f64() * 1e6 / chunks.len() as f64,
            (per_column.as_secs_f64() - per_chunk.as_secs_f64()) * 1e6 / chunks.len() as f64,
        );
        assert!(
            per_chunk < per_column,
            "resolving the bucket once per chunk ({per_chunk:?}) was not cheaper than once per \
             column ({per_column:?})"
        );

        // ---- (2) the rock pass: a localised lookup, not a scan of the index
        //
        // Measured twice, over two deliberately different rock populations,
        // because the two answer different questions and only reporting one
        // of them would mislead:
        //
        // * **uniform** -- rocks from chunks spread over the whole map, which is where
        //   `apply_rocks_to` actually spends its time. The authored region is a percent
        //   or two of Cromatolis, so this is what the change is worth in production.
        // * **in-footprint** -- rocks from the authored-void chunks only. The worst
        //   case, where the locality test cannot reject anything and only the tier
        //   summary saves work.
        let params = TraversalParams::ENGINE;
        let lattice = rock::rock_lattice(info.index().seed);
        let rocks_in = |sample: &[Vec2<i32>]| -> Vec<Aabr<i32>> {
            sample
                .iter()
                .flat_map(|cpos| {
                    let min = cpos.cpos_to_wpos();
                    let max = min + chunk_size;
                    lattice
                        .iter(min, max)
                        .filter(move |(wpos, _)| {
                            wpos.x >= min.x && wpos.y >= min.y && wpos.x < max.x && wpos.y < max.y
                        })
                        .filter_map(|(wpos, seed)| {
                            let col = info.col_or_gen(wpos)?;
                            let rock = rock::rock_at(wpos, seed, col.as_ref())?;
                            Some(analysis_footprint(rock.world_bounds(), &params))
                        })
                        .collect::<Vec<_>>()
                })
                .collect()
        };

        // Hashed rather than strided: `(i * 97, i * 101)` is linear in `i` in
        // both coordinates, i.e. a diagonal line through chunk space rather
        // than a sample of the map, which is not what "uniform" should mean in
        // the line this prints.
        let scatter = RandomField::new(0xC0_23_00_01);
        let map_chunks = info.chunks().get_size().map(|e| e as i32);
        let uniform_chunks: Vec<Vec2<i32>> = (0..CHUNK_SAMPLE as u32)
            .map(|i| {
                Vec2::new(
                    (scatter.get(Vec3::new(i as i32, 0, 0)) % map_chunks.x as u32) as i32,
                    (scatter.get(Vec3::new(0, i as i32, 1)) % map_chunks.y as u32) as i32,
                )
            })
            .collect();

        let tiers = [AccommodationTier::Catalog, AccommodationTier::HandAuthored];
        let measure = |name: &str, sample: &[Vec2<i32>]| {
            let rocks = rocks_in(sample);
            assert!(
                !rocks.is_empty(),
                "{name}: the lattice placed no rock at all over {} chunks; this half of the \
                 measurement is not looking at the rock pass",
                sample.len()
            );

            // The shape the lookup had before: a linear scan over every
            // indexed shape, per tier, per rock.
            let mut sink = 0_u64;
            let start = Instant::now();
            for _ in 0..ROCK_REPEATS {
                for _ in &rocks {
                    let built: Vec<_> = tiers
                        .into_iter()
                        .filter_map(|tier| voids.passage_by_scan(info, tier))
                        .collect();
                    sink += built.len() as u64;
                }
            }
            let scanned = start.elapsed();

            let mut sink_near = 0_u64;
            let start = Instant::now();
            for _ in 0..ROCK_REPEATS {
                for footprint in &rocks {
                    let built: Vec<_> = tiers
                        .into_iter()
                        .filter_map(|tier| voids.passage_near(info, tier, *footprint))
                        .collect();
                    sink_near += built.len() as u64;
                }
            }
            let localised = start.elapsed();

            // How many rocks the localised form answers "no authored passage"
            // for outright. Each of those also skips the coarse-band
            // pre-check the old form then had to run over its whole
            // footprint, which is a larger saving than the lookup itself and
            // is *not* in the timings above.
            let skipped = rocks
                .iter()
                .filter(|footprint| {
                    tiers
                        .into_iter()
                        .all(|tier| voids.passage_near(info, tier, **footprint).is_none())
                })
                .count();
            let lookups = u64::from(ROCK_REPEATS) * rocks.len() as u64;
            println!(
                "rock pass authored-passage lookup ({name}), {} real rocks over {} chunks, \
                 x{ROCK_REPEATS}:\n    index scan + passage Vec, per rock: {scanned:?} ({:.0} \
                 ns/rock)\n    localised to the analysis footprint: {localised:?} ({:.0} \
                 ns/rock)\n    rocks that now build no authored passage at all: {skipped}/{} \
                 ({:.1} %)\n    passages built per pass: {} scanned vs {} localised [sink \
                 {sink}/{sink_near}]",
                rocks.len(),
                sample.len(),
                scanned.as_secs_f64() * 1e9 / lookups as f64,
                localised.as_secs_f64() * 1e9 / lookups as f64,
                rocks.len(),
                skipped as f64 * 100.0 / rocks.len() as f64,
                sink / u64::from(ROCK_REPEATS),
                sink_near / u64::from(ROCK_REPEATS),
            );
            assert!(
                localised < scanned,
                "{name}: the localised lookup ({localised:?}) was not cheaper than the index scan \
                 ({scanned:?})"
            );
            assert!(
                sink_near <= sink,
                "{name}: the localised lookup built MORE passages than the index scan; it is \
                 supposed to be the same question asked of fewer shapes, never a wider one"
            );
        };
        measure("uniform", &uniform_chunks);
        measure("in-footprint", &chunks);
    });
}

/// How much rock an authored generic cave actually leaves between itself and
/// the sky.
///
/// The three constants that decide this — the branch-floor clamp, the
/// per-size-class carve depth, and `authored_voids::SURFACE_MARGIN` — were
/// each chosen on their own terms, and the number they produce *together* on
/// this landmass had never been measured. It turned out to be three or four
/// blocks across most of a Giant cave's footprint: not a cave with a roof, a
/// hollow under a crust. This is the measurement that says so, and the
/// regression guard that keeps it from silently going back.
///
/// Read it as a *shape* measurement, not a count of defects. The sample is
/// one 256x256-block window centred on `cave.cave_020` (a Giant, and the cave
/// whose in-client report started this), so the absolute numbers belong to
/// that window; what matters is the fraction, and that nothing breaches.
///
/// Two measurement details that are load-bearing rather than incidental:
///
/// * The surface is found by scanning down from `col.alt`, not from the top of
///   the chunk. Scanning from the top finds a tree canopy or a boulder and then
///   reads the gap under the leaves as a breached roof — that error alone
///   reported 274 false breaches in this window.
/// * `col.alt` is the same altitude the carve caps its ceiling against, and it
///   tracks the real generated ground closely (measured over this window:
///   median +0.3 blocks, never more than +1.0). So a thin roof here is the
///   carve doing what it was told, not the carve being told the wrong height.
///
/// `cargo test -p xindeler-world --release
/// authored_cave_roofs_are_rock_not_crust -- --ignored --nocapture`
#[test]
#[ignore]
fn authored_cave_roofs_are_rock_not_crust() {
    use common::{
        terrain::{CoordinateConversions, TerrainChunk, TerrainChunkSize},
        vol::{ReadVol, RectVolSize},
    };

    /// Centre of the sample window: inside `cave.cave_020`'s footprint.
    const WINDOW_CENTRE: Vec2<i32> = Vec2 {
        x: 10_937,
        y: 30_535,
    };
    /// Half-width of the sample window, in blocks.
    const WINDOW_HALF: i32 = 128;
    /// At or below this much rock cover, the ground reads as a crust rather
    /// than a cave roof: a few blocks is what a player breaks through by
    /// accident.
    const CRUST_BLOCKS: i32 = 4;

    let threadpool = rayon::ThreadPoolBuilder::new().build().unwrap();
    let (world, index) = World::generate(
        0,
        sim::WorldOpts {
            seed_elements: true,
            world_file: sim::FileOpts::LoadAsset("world.map.cromatolis_v0".to_string()),
            calendar: None,
        },
        &threadpool,
        &|_| {},
    );
    let index_ref = index.as_index_ref();
    let sim = world.sim();

    let mut chunks: std::collections::HashMap<Vec2<i32>, TerrainChunk> =
        std::collections::HashMap::new();
    let mut columns = 0_u32;
    let mut breached = 0_u32;
    let mut crust = 0_u32;

    CanvasInfo::with_mock_canvas_info(index_ref, sim, |info| {
        for dy in -WINDOW_HALF..WINDOW_HALF {
            for dx in -WINDOW_HALF..WINDOW_HALF {
                let wpos = WINDOW_CENTRE + Vec2::new(dx, dy);
                let cpos = wpos.wpos_to_cpos();
                if let std::collections::hash_map::Entry::Vacant(slot) = chunks.entry(cpos) {
                    let Ok((chunk, _)) =
                        world.generate_chunk(index_ref, cpos, None, || false, None, None)
                    else {
                        continue;
                    };
                    slot.insert(chunk);
                }
                let chunk = &chunks[&cpos];
                let rel = Vec2::new(
                    wpos.x - cpos.x * TerrainChunkSize::RECT_SIZE.x as i32,
                    wpos.y - cpos.y * TerrainChunkSize::RECT_SIZE.y as i32,
                );
                let Some(col) = info.col_or_gen(wpos) else {
                    continue;
                };
                let bottom = chunk.get_min_z();
                let ground = col.alt.floor() as i32;
                let Some(surface) = (bottom..=ground)
                    .rev()
                    .find(|&z| chunk.get(rel.with_z(z)).is_ok_and(|b| b.is_filled()))
                else {
                    continue;
                };
                columns += 1;
                let Some(void_top) = (bottom..surface)
                    .rev()
                    .find(|&z| chunk.get(rel.with_z(z)).is_ok_and(|b| !b.is_filled()))
                else {
                    continue;
                };
                let cover = surface - void_top;
                if cover <= 1 {
                    breached += 1;
                }
                if cover <= CRUST_BLOCKS {
                    crust += 1;
                }
            }
        }
    });

    let crust_fraction = f64::from(crust) / f64::from(columns);
    println!(
        "cave_020 window: {columns} ground columns, {breached} breached, {crust} roofed by \
         {CRUST_BLOCKS} blocks or less ({:.1}%)",
        crust_fraction * 100.0
    );

    assert_eq!(
        breached, 0,
        "an authored cave reached the sky. `SURFACE_MARGIN` is supposed to make that impossible, \
         so this is the cap itself failing rather than a tuning question"
    );
    // Measured 8.8% after the branch-floor clamp was raised from 24 to 64,
    // against 23.8% before it. The bound sits well above the measurement and
    // well below the old value on purpose: it is here to catch a regression
    // back towards "the whole footprint is a crust", not to pin a number that
    // moves whenever the terrain does.
    assert!(
        crust_fraction < 0.15,
        "{:.1}% of this window is cave roofed by {CRUST_BLOCKS} blocks or less. That was 23.8% \
         when the branch-floor clamp kept tunnels at the hub's altitude while the ground fell \
         away beneath them; if it is back up there, check `MAX_BRANCH_FLOOR_DRIFT` and the \
         `*_DEPTH` constants before anything else",
        crust_fraction * 100.0
    );
}

/// The bug this guards against: Cromatolis's 7 authored `Inn` (and any
/// `Post`) settlements resolve to `SiteKind::Camp` as a physical stand-in --
/// no dedicated inn/post generator exists yet (see
/// `civ::resolve_settlement_site_kind`). `Camp` is also what genuine
/// procedural bandit camps use, and `SiteKind::marker` used to return `None`
/// for every `Camp` unconditionally, so none of the 7 inns showed up on the
/// world map at all -- unlike every other settlement category.
///
/// `civ::Site.authored.is_some()` (surfaced on the real, generated
/// `site::Site` as `is_authored_settlement`) is the same signal
/// `rtsim::generate::site::good_or_evil` already uses to keep these sites
/// from being classified as hostile (COW-19); this test is that fix's
/// counterpart for the map marker, exercised against the real generated
/// world rather than the `SiteKind::marker` unit test's synthetic bool.
///
/// `cargo test -p xindeler-world
/// cromatolis_authored_inns_and_posts_get_a_town_marker -- --ignored`
#[test]
#[ignore]
fn cromatolis_authored_inns_and_posts_get_a_town_marker() {
    let threadpool = rayon::ThreadPoolBuilder::new().build().unwrap();
    let (world, index) = World::generate(
        0,
        sim::WorldOpts {
            seed_elements: true,
            world_file: sim::FileOpts::LoadAsset("world.map.cromatolis_v0".to_string()),
            calendar: None,
        },
        &threadpool,
        &|_| {},
    );
    let index_ref = index.as_index_ref();

    // Every real `WorldSite` that is both `SiteKind::Camp` and
    // `is_authored_settlement` is one of Cromatolis's authored inns/posts:
    // `Camp` is currently the physical stand-in for exactly those two
    // categories (see `civ::AuthoredSettlementCategory::default_site_kind`),
    // and nothing else in this region sets `is_authored_settlement` on a
    // `Camp` site. Found this way (rather than by name) so the assertion
    // doesn't quietly stop covering a settlement if the authored export
    // renames or adds one.
    let authored_camp_sites: Vec<_> = index_ref
        .sites
        .iter()
        .filter(|(_, site)| site.kind == Some(SiteKind::Camp) && site.is_authored_settlement)
        .map(|(id, site)| (id, site.name().unwrap_or("<unnamed>").to_string()))
        .collect();

    assert!(
        authored_camp_sites.len() >= 7,
        "expected at least the 7 reviewed authored inns among Cromatolis's Camp-stand-in sites, \
         found {}: {authored_camp_sites:?}",
        authored_camp_sites.len()
    );

    let map_data = world.get_map_data(index_ref, &threadpool);
    let town_marker_site_ids: std::collections::HashSet<_> = map_data
        .sites
        .iter()
        .filter(|marker| marker.kind == MarkerKind::Town)
        .filter_map(|marker| marker.site)
        .collect();

    for (site_id, name) in &authored_camp_sites {
        assert!(
            town_marker_site_ids.contains(&site_id.id()),
            "authored settlement {name:?} ({site_id:?}) resolves to SiteKind::Camp but got no \
             Town marker on the world map -- `SiteKind::marker` regressed back to treating every \
             Camp as unmarked regardless of is_authored_settlement"
        );
    }

    // A genuine wild procedural `Camp` must still get no marker at all --
    // this only matters outside Cromatolis (see the comment on
    // `civs_generate_places_every_authored_cromatolis_settlement_and_landmark`
    // in `civ::mod::tests`: an authored-settlements region places zero
    // procedural minor sites, so Cromatolis itself has none to check here),
    // and is covered directly by the `SiteKind::marker` unit tests in
    // `site::mod::tests` instead of by asset-dependent generation.
}

/// The authored settlements that appear as `Site(...)` stops of an authored
/// maritime route, and therefore the naval-port tier their authored category
/// resolves to. Spelled out here rather than derived, so the test states the
/// expectation instead of restating the implementation it is checking.
///
/// **This is a golden list and has to be updated by hand** whenever
/// `assets/world/map/cromatolis_v0_maritime_routes.ron` gains or loses a
/// `Site(...)` stop, or a settlement's `category` changes in
/// `cromatolis_v0_sites.ron`. It fails loudly rather than silently when it goes
/// stale -- a stop that was added shows up as a `stray` port, one that was
/// removed as a `missing` one, and a re-tiered settlement trips the per-class
/// assertion -- but the message will name the settlement, not this constant, so
/// start here.
const EXPECTED_NAVAL_PORTS: &[(&str, PortClass)] = &[
    ("site.kalthis", PortClass::Harbour),
    ("site.dove_city", PortClass::Quay),
    ("site.dromos_city", PortClass::Quay),
    ("site.bronze_shore", PortClass::Pier),
    ("site.kalitos", PortClass::Pier),
    ("site.sutar_town", PortClass::Pier),
    ("site.mazon_town", PortClass::Pier),
    ("site.portland", PortClass::Pier),
    ("site.rios_port", PortClass::Pier),
    ("site.andiran", PortClass::Jetty),
    ("site.garens_town", PortClass::Jetty),
    ("site.hita", PortClass::Jetty),
    ("site.timos", PortClass::Jetty),
];

/// Chebyshev tile distance from `from` to the nearest `is_road()` tile of
/// `site`'s grid, brute-forced over the grid's own bounds. Only used to
/// report what the placement actually achieved, so clarity beats speed.
fn nearest_road_tiles(site: &site::Site, from: Vec2<i32>) -> Option<i32> {
    let bounds = site.tiles.bounds;
    let mut best = None;
    for y in bounds.min.y..=bounds.max.y {
        for x in bounds.min.x..=bounds.max.x {
            let tpos = Vec2::new(x, y);
            if site.tiles.get(tpos).is_road() {
                let d = (tpos - from).map(|e| e.abs()).reduce_max();
                best = Some(best.map_or(d, |b: i32| b.min(d)));
            }
        }
    }
    best
}

/// How many `TileKind::Pier` tiles a site's grid carries.
fn pier_tile_count(site: &site::Site) -> usize {
    let bounds = site.tiles.bounds;
    let mut count = 0;
    for y in bounds.min.y..=bounds.max.y {
        for x in bounds.min.x..=bounds.max.x {
            if site.tiles.get(Vec2::new(x, y)).is_pier() {
                count += 1;
            }
        }
    }
    count
}

/// **The waterfront placement pass, against the real authored region.**
///
/// Requires the real Cromatolis LFS assets to be pulled locally (`git lfs
/// pull` against the VPS store); `#[ignore]`d for the same reason every other
/// test in this file is. Recommended command:
/// `cargo test -p xindeler-world
/// every_authored_maritime_route_stop_gets_a_naval_port_footprint -- --ignored
/// --nocapture`
///
/// Reports, per settlement, whether a footprint was claimed and where, so the
/// numbers can be read against the measurement pass that gated this work --
/// and asserts the four things that would otherwise be silent failures:
///
/// * every one of the 13 route stops gets a footprint, of the tier its authored
///   category implies (a settlement that fails names itself and its reason in
///   the generation log, and the assertion message below lists it);
/// * the deck was blitted whole -- exactly one `TileKind::Pier` tile per tile
///   of the recorded rectangle, so nothing was clipped or overlapped;
/// * each deck reaches water deep enough for its own tier's berths, rather than
///   ending on the beach;
/// * no settlement that was never asked for a port has a single
///   `TileKind::Pier` tile, which is what "non-port settlements are untouched"
///   reduces to once `place_naval_port` is only reachable behind `naval_port:
///   Some(_)` and the `None` path consumes no RNG (pinned separately by
///   `site::tests::an_unviable_waterfront_places_no_port_and_no_fallback`).
#[test]
#[ignore]
fn every_authored_maritime_route_stop_gets_a_naval_port_footprint() {
    let threadpool = rayon::ThreadPoolBuilder::new().build().unwrap();
    let (world, index) = World::generate(
        0,
        sim::WorldOpts {
            seed_elements: true,
            world_file: sim::FileOpts::LoadAsset("world.map.cromatolis_v0".to_string()),
            calendar: None,
        },
        &threadpool,
        &|_| {},
    );
    let index_ref = index.as_index_ref();

    let expected: std::collections::HashMap<&str, PortClass> =
        EXPECTED_NAVAL_PORTS.iter().copied().collect();

    let mut placed = Vec::new();
    let mut missing = Vec::new();
    let mut stray = Vec::new();
    let mut beached = Vec::new();

    for civ_site in world.civs.sites.values() {
        let Some(authored_id) = civ_site.authored_id() else {
            continue;
        };
        let Some(site_id) = civ_site.site_tmp else {
            continue;
        };
        let site = index_ref.sites.get(site_id);
        let pier_count = pier_tile_count(site);

        match (expected.get(authored_id).copied(), site.naval_port) {
            (Some(class), Some(placement)) => {
                assert_eq!(
                    placement.class, class,
                    "{authored_id} got a {:?} port, expected {class:?}",
                    placement.class
                );
                let deck_tiles = placement.deck.size().product() as usize;
                assert_eq!(
                    pier_count, deck_tiles,
                    "{authored_id} recorded a {deck_tiles}-tile {class:?} deck but its grid \
                     carries {pier_count} Pier tiles"
                );
                let apron_face_centre = placement.hinge.center();
                let d_centre_blocks = site
                    .tile_center_wpos(apron_face_centre)
                    .as_::<f32>()
                    .distance(site.origin.as_::<f32>());
                let d_road = nearest_road_tiles(site, placement.door_tile);
                // A deck that stopped on the beach is what this catches: the
                // placement records how much of the deck's centre line was over
                // adequately deep water. Asserting the *recorded* figure rather
                // than re-measuring the terrain is the point -- re-measuring
                // would test the terrain again, where what is in question is
                // whether the search's own gate was actually applied.
                if placement.deck_deep_tiles < site::SHORE_MIN_DEEP_TILES {
                    beached.push((authored_id.to_string(), placement.deck_deep_tiles));
                }
                placed.push((
                    authored_id.to_string(),
                    class,
                    placement.apron.size(),
                    placement.deck.size(),
                    placement.outward,
                    d_road,
                    d_centre_blocks,
                    pier_count,
                    placement.causeway,
                    placement.deck_deep_tiles,
                    placement.score,
                ));
            },
            (Some(class), None) => missing.push((authored_id.to_string(), class)),
            // A settlement that was never asked for a port must have neither a
            // recorded placement nor a claimed tile. Both halves are checked
            // unconditionally -- gating the recorded-placement half on the tile
            // count would let a port that recorded itself and blitted nothing
            // pass silently.
            (None, Some(_)) => stray.push((authored_id.to_string(), pier_count)),
            (None, None) if pier_count > 0 => stray.push((authored_id.to_string(), pier_count)),
            (None, None) => {},
        }
    }

    placed.sort_by(|a, b| a.0.cmp(&b.0));
    println!(
        "\n{:<20} {:<8} {:>9} {:>9} {:>8} {:>6} {:>6} {:>9} {:>4} {:>6} {:>5} {:>5}",
        "settlement",
        "tier",
        "apron",
        "deck",
        "outward",
        "cswy",
        "deep",
        "d_centre",
        "road",
        "doorrd",
        "run",
        "altv",
    );
    for (id, class, apron, deck, outward, door_road, d_centre, piers, causeway, deep, score) in
        &placed
    {
        println!(
            "{:<20} {:<8} {:>4}x{:<4} {:>4}x{:<4} {:>4},{:<3} {:>6} {:>6} {:>9.1} {:>4} {:>6} \
             {:>5} {:>5.1}  ({piers} pier tiles)",
            id,
            format!("{class:?}"),
            apron.w,
            apron.h,
            deck.w,
            deck.h,
            outward.x,
            outward.y,
            causeway,
            deep,
            d_centre,
            score.d_road,
            door_road.map_or("-".to_string(), |d| d.to_string()),
            score.water_run,
            score.alt_var,
        );
    }
    println!();

    assert!(
        missing.is_empty(),
        "{} of {} authored maritime route stops got no naval port footprint: {missing:?} -- the \
         generation log names each failure's reason",
        missing.len(),
        EXPECTED_NAVAL_PORTS.len(),
    );
    assert_eq!(
        placed.len(),
        EXPECTED_NAVAL_PORTS.len(),
        "expected all {} route stops to be present as generated settlements, found {}",
        EXPECTED_NAVAL_PORTS.len(),
        placed.len(),
    );
    assert!(
        stray.is_empty(),
        "settlements with no maritime route stop must be untouched by the placement pass, but \
         these carry Pier tiles: {stray:?}"
    );

    assert!(
        beached.is_empty(),
        "these decks do not reach {} tiles of water deep enough for their own tier's berths, so \
         they end on the beach: {beached:?}",
        site::SHORE_MIN_DEEP_TILES
    );
}

/// **Real, walkable dock geometry -- not just the tile claim.**
///
/// `every_authored_maritime_route_stop_gets_a_naval_port_footprint` (above)
/// already covers the tile-grid claim every one of the 13 route stops makes.
/// This test covers the layer built on top of it: that the `Jetty`/`Pier`
/// settlements actually construct a `PlotKind::NavalPort` plot -- real
/// `Painter` geometry a player can walk to -- and not just the bare
/// apron/deck tiles. `Quay`/`Harbour` (Kalthis, Dove City, Dromos City) are
/// excluded on purpose: no `generate_city` call site builds a plot for those
/// two tiers yet, so their tiles are expected to stay plot-less.
///
/// Requires the real Cromatolis LFS assets to be pulled locally. Recommended
/// command: `cargo test -p xindeler-world
/// the_13_route_stops_all_generate_a_naval_port -- --ignored --nocapture`
#[test]
#[ignore]
fn the_13_route_stops_all_generate_a_naval_port() {
    let threadpool = rayon::ThreadPoolBuilder::new().build().unwrap();
    let (world, index) = World::generate(
        0,
        sim::WorldOpts {
            seed_elements: true,
            world_file: sim::FileOpts::LoadAsset("world.map.cromatolis_v0".to_string()),
            calendar: None,
        },
        &threadpool,
        &|_| {},
    );
    let index_ref = index.as_index_ref();

    let jetty_or_pier: std::collections::HashMap<&str, PortClass> = EXPECTED_NAVAL_PORTS
        .iter()
        .copied()
        .filter(|(_, class)| matches!(class, PortClass::Jetty | PortClass::Pier))
        .collect();

    let mut built = Vec::new();
    let mut missing = Vec::new();

    for civ_site in world.civs.sites.values() {
        let Some(authored_id) = civ_site.authored_id() else {
            continue;
        };
        let Some(&class) = jetty_or_pier.get(authored_id) else {
            continue;
        };
        let Some(site_id) = civ_site.site_tmp else {
            missing.push((
                authored_id.to_string(),
                "settlement did not generate a site".to_string(),
            ));
            continue;
        };
        let site = index_ref.sites.get(site_id);

        let Some(port) = site.plots().find_map(|plot| match plot.kind() {
            PlotKind::NavalPort(port) => Some(port),
            _ => None,
        }) else {
            missing.push((authored_id.to_string(), "no NavalPort plot".to_string()));
            continue;
        };

        assert_eq!(
            port.class, class,
            "{authored_id} built a {:?} NavalPort plot, expected {class:?}",
            port.class
        );

        // The plot's own geometry has to be built from the same footprint
        // the placement pass recorded, not a second, independently-derived
        // one -- otherwise the plot and the tile claim could silently drift
        // apart.
        let placement = site.naval_port.unwrap_or_else(|| {
            panic!("{authored_id} has a NavalPort plot but no recorded ShorePlacement")
        });
        let to_wpos_aabr = |aabr: Aabr<i32>| Aabr {
            min: site.tile_wpos(aabr.min),
            max: site.tile_wpos(aabr.max),
        };
        assert_eq!(
            port.apron,
            to_wpos_aabr(placement.apron),
            "{authored_id}'s plot apron does not match the placement it was built from"
        );
        assert_eq!(
            port.deck,
            to_wpos_aabr(placement.deck),
            "{authored_id}'s plot deck does not match the placement it was built from"
        );

        assert!(
            Structure::door_tile(port).is_some(),
            "{authored_id}'s NavalPort plot must report a door tile"
        );

        built.push((
            authored_id.to_string(),
            port.class,
            placement.deck.size().product(),
            placement.apron.size().product(),
            port.water_alt,
            port.alt,
        ));
    }

    built.sort_by(|a, b| a.0.cmp(&b.0));
    println!(
        "\n{:<16} {:<6} {:>10} {:>11} {:>9} {:>5}",
        "settlement", "tier", "deck_tiles", "apron_tiles", "water_alt", "alt"
    );
    for (id, class, deck_tiles, apron_tiles, water_alt, alt) in &built {
        println!(
            "{id:<16} {:<6} {deck_tiles:>10} {apron_tiles:>11} {water_alt:>9} {alt:>5}",
            format!("{class:?}"),
        );
    }

    assert!(
        missing.is_empty(),
        "{} of {} Jetty/Pier route stops did not build a real NavalPort plot: {missing:?}",
        missing.len(),
        jetty_or_pier.len(),
    );
    assert_eq!(
        built.len(),
        jetty_or_pier.len(),
        "expected all {} Jetty/Pier settlements to build a NavalPort plot, found {}",
        jetty_or_pier.len(),
        built.len(),
    );
}
