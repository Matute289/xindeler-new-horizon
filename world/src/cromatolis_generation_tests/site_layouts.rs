//! Per-site layout digests for the authored Cromatolis world: a regression
//! guard that makes the cost of a terrain edit visible, plus the terrain
//! perturbation experiments that measure how many towns an edit re-rolls.
//!
//! A site's *layout digest* is a SHA-256 over everything that makes up its
//! generated layout: name, origin, radius, every plot (kind, root tile, tile
//! count, bounds) and, for a naval port, its placement and its berths and
//! anchorages (with port-local ids, so another port gaining a berth can't
//! leak into this one's digest). Sites are keyed by
//! [`crate::civ::seeds::site_seed_key`] -- the same stable identity their
//! seed is derived from -- never by generation order.
//!
//! Everything here needs the real Cromatolis LFS assets and is `#[ignore]`d.
//! To re-record the committed file after a deliberate change (a terrain
//! edit whose re-rolls were reviewed, or an engine change):
//!
//! ```text
//! XINDELER_BLESS_SITE_LAYOUTS=1 VELOREN_ASSETS=... cargo test -p xindeler-world \
//!     --release --lib site_layouts::cromatolis_site_layout_digests_match -- --ignored
//! ```
//!
//! To measure an arbitrary edit before making it for real, see
//! [`perturbation_experiment_from_env`].

use super::*;
use crate::{
    civ::seeds::site_seed_key,
    sim::test_hooks::{MapPerturbation, set_loaded_map_perturbation},
};
use common::terrain::vec2_as_uniform_idx;
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    fmt::Write as _,
    sync::{Arc, OnceLock},
};

/// Committed digests, relative to this crate's root.
const DIGEST_FILE: &str = "src/cromatolis_site_layout_digests.txt";
/// Committed digest of a procedural (non-authored) world sample.
const PROCEDURAL_DIGEST_FILE: &str = "src/procedural_world_civ_digest.txt";
const BLESS_ENV: &str = "XINDELER_BLESS_SITE_LAYOUTS";

// ---------------------------------------------------------------------------
// Terrain perturbation (through `sim::test_hooks`)
// ---------------------------------------------------------------------------

/// Raise (or lower) a set of chunks, or the whole map, by `dz` blocks --
/// both the surface and the basement, i.e. a rigid vertical shift of that
/// column, which is what a heightmap edit is.
fn raise_chunks(chunks: Vec<Vec2<i32>>, dz: f64) -> MapPerturbation {
    Arc::new(move |size, alt, basement| {
        for &chunk in &chunks {
            let i = vec2_as_uniform_idx(size, chunk);
            alt[i] += dz;
            basement[i] += dz;
        }
    })
}

fn raise_everything(dz: f64) -> MapPerturbation {
    Arc::new(move |_, alt, basement| {
        alt.iter_mut().for_each(|a| *a += dz);
        basement.iter_mut().for_each(|b| *b += dz);
    })
}

fn generate_world(map: &str, perturbation: Option<MapPerturbation>) -> (World, IndexOwned) {
    let mut builder = rayon::ThreadPoolBuilder::new();
    if let Some(perturbation) = perturbation {
        builder = builder.start_handler(move |_| {
            set_loaded_map_perturbation(Some(Arc::clone(&perturbation)));
        });
    }
    let threadpool = builder.build().unwrap();
    World::generate(
        0,
        sim::WorldOpts {
            seed_elements: true,
            world_file: sim::FileOpts::LoadAsset(map.to_string()),
            calendar: None,
        },
        &threadpool,
        &|_| {},
    )
}

fn generate_cromatolis(perturbation: Option<MapPerturbation>) -> (World, IndexOwned) {
    generate_world("world.map.cromatolis_v0", perturbation)
}

// ---------------------------------------------------------------------------
// Digests
// ---------------------------------------------------------------------------

#[derive(Clone, Debug, PartialEq, Eq)]
struct SiteLayout {
    name: String,
    digest: String,
    plots: usize,
    /// Plots that are not plazas, roads, farm fields or bridges.
    buildings: usize,
    /// `Kind:count` per plot kind, sorted.
    kinds: String,
    /// Naval port summary (berth classes, sorted, and anchorage count), or
    /// `-`. Already covered by the digest; kept readable for reports.
    port: String,
    /// Authored settlement `(category, size)` contract keys, if any.
    authored_size: Option<(&'static str, &'static str)>,
}

/// Everything layout-relevant about one world: per-site digests keyed by
/// stable site key, plus a digest of the road network.
#[derive(Clone, Debug, PartialEq, Eq)]
struct WorldLayouts {
    sites: BTreeMap<String, SiteLayout>,
    roads: RoadSummary,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct RoadSummary {
    tracks: usize,
    nodes: usize,
    digest: String,
    /// Node count per track, keyed by its sorted node list's first/last
    /// node (tracks have no stable name of their own).
    per_track: BTreeMap<String, usize>,
    /// Every road's node list.
    paths: BTreeSet<Vec<(i32, i32)>>,
}

fn sha16(text: &str) -> String {
    Sha256::digest(text.as_bytes())
        .iter()
        .take(8)
        .fold(String::new(), |mut s, b| {
            let _ = write!(s, "{b:02x}");
            s
        })
}

fn site_layout(site: &site::Site) -> SiteLayout {
    let mut canonical = String::new();
    let _ = writeln!(
        canonical,
        "name={:?}\norigin={},{}\nradius={:08x}",
        site.name(),
        site.origin.x,
        site.origin.y,
        site.radius().to_bits()
    );
    let mut plots: Vec<String> = site
        .plots()
        .map(|plot| {
            let b = plot.find_bounds();
            format!(
                "{} root={},{} tiles={} bounds={},{},{},{}",
                plot.kind(),
                plot.root_tile().x,
                plot.root_tile().y,
                plot.tiles().len(),
                b.min.x,
                b.min.y,
                b.max.x,
                b.max.y
            )
        })
        .collect();
    plots.sort();
    for plot in &plots {
        let _ = writeln!(canonical, "{plot}");
    }
    // The port's footprint only: `ShorePlacement` also carries float
    // diagnostics of the search (`alt_var`, `cost`, ...) that drift with any
    // sub-block altitude change without the port moving at all.
    if let Some(port) = site.naval_port.as_ref() {
        let _ = writeln!(
            canonical,
            "port={:?} apron={:?} deck={:?} hinge={:?} door={:?} outward={:?}",
            port.class, port.apron, port.deck, port.hinge, port.door_tile, port.outward
        );
    }
    if let Some(info) = site.plots().find_map(|plot| plot.naval_dock_info()) {
        let _ = writeln!(
            canonical,
            "dock={:?} {:?} {:?}\nberths={:?}\nanchorages={:?}",
            info.class, info.center, info.door_tile, info.berths, info.anchorages
        );
    }

    let mut kinds: BTreeMap<String, usize> = BTreeMap::new();
    for plot in site.plots() {
        *kinds.entry(plot.kind().to_string()).or_default() += 1;
    }
    let buildings = crate::civ::seeds::building_count(site);
    SiteLayout {
        name: site.name().unwrap_or("-").to_string(),
        digest: sha16(&canonical),
        plots: site.plots().len(),
        buildings,
        kinds: kinds
            .iter()
            .map(|(k, n)| format!("{k}:{n}"))
            .collect::<Vec<_>>()
            .join(" "),
        port: site.plots().find_map(|plot| plot.naval_dock_info()).map_or(
            "-".to_string(),
            |info| {
                let mut classes: Vec<String> = info
                    .berths
                    .iter()
                    .map(|b| format!("{:?}", b.class))
                    .collect();
                classes.sort();
                format!(
                    "{:?}[{}]+{}anch",
                    info.class,
                    classes.join(","),
                    info.anchorages.len()
                )
            },
        ),
        authored_size: None,
    }
}

fn world_layouts(world: &World, index: &IndexOwned) -> WorldLayouts {
    let index_ref = index.as_index_ref();
    let mut sites = BTreeMap::new();
    for civ_site in world.civs.sites.values() {
        let key = site_seed_key(civ_site);
        let Some(site_id) = civ_site.site_tmp else {
            panic!("{key} has no generated site");
        };
        let layout = SiteLayout {
            authored_size: civ_site.authored_category_and_size(),
            ..site_layout(index_ref.sites.get(site_id))
        };
        assert!(
            sites.insert(key.clone(), layout).is_none(),
            "two sites share the stable key {key}"
        );
    }

    let mut paths: Vec<Vec<(i32, i32)>> = world
        .civs
        .tracks
        .iter()
        .map(|(_, track)| track.path().nodes().iter().map(|n| (n.x, n.y)).collect())
        .collect();
    paths.sort();
    let mut canonical = String::new();
    let mut per_track = BTreeMap::new();
    for path in &paths {
        let _ = writeln!(canonical, "{path:?}");
        let (first, last) = (path.first().unwrap(), path.last().unwrap());
        let mut key = format!("{},{}->{},{}", first.0, first.1, last.0, last.1);
        while per_track.contains_key(&key) {
            key.push('\'');
        }
        per_track.insert(key, path.len());
    }
    WorldLayouts {
        sites,
        roads: RoadSummary {
            tracks: paths.len(),
            nodes: paths.iter().map(Vec::len).sum(),
            digest: sha16(&canonical),
            per_track,
            paths: paths.iter().cloned().collect(),
        },
    }
}

/// The unperturbed Cromatolis world's layouts, generated once per test
/// process and shared by every test that compares against it.
fn base_layouts() -> &'static WorldLayouts {
    static BASE: OnceLock<WorldLayouts> = OnceLock::new();
    BASE.get_or_init(|| {
        let (world, index) = generate_cromatolis(None);
        world_layouts(&world, &index)
    })
}

fn render_digest_file(layouts: &WorldLayouts) -> String {
    let mut out = String::new();
    out.push_str(
        "# Per-site layout digests of the authored Cromatolis world (world seed 0).\n# Checked by \
         world/src/cromatolis_generation_tests/site_layouts.rs; re-record with\n# \
         XINDELER_BLESS_SITE_LAYOUTS=1 (see that module's doc) only after reviewing\n# which \
         sites changed and why.\n#\n# key\tdigest\tplots\tbuildings\tname\n",
    );
    let _ = writeln!(
        out,
        "@roads\t{}\t{}\t{}\t-",
        layouts.roads.digest, layouts.roads.tracks, layouts.roads.nodes
    );
    for (key, site) in &layouts.sites {
        let _ = writeln!(
            out,
            "{key}\t{}\t{}\t{}\t{}",
            site.digest, site.plots, site.buildings, site.name
        );
    }
    out
}

/// `key -> (digest, plots, buildings, name)` from a committed digest file.
fn parse_digest_file(text: &str) -> BTreeMap<String, (String, usize, usize, String)> {
    text.lines()
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .map(|line| {
            let cols: Vec<&str> = line.split('\t').collect();
            assert_eq!(cols.len(), 5, "malformed digest line: {line:?}");
            (
                cols[0].to_string(),
                (
                    cols[1].to_string(),
                    cols[2].parse().unwrap(),
                    cols[3].parse().unwrap(),
                    cols[4].to_string(),
                ),
            )
        })
        .collect()
}

/// Keys whose layout differs between two worlds (including a site present
/// in only one of them).
fn changed_sites(a: &WorldLayouts, b: &WorldLayouts) -> Vec<String> {
    let keys: BTreeSet<&String> = a.sites.keys().chain(b.sites.keys()).collect();
    keys.into_iter()
        .filter(|key| a.sites.get(*key) != b.sites.get(*key))
        .cloned()
        .collect()
}

fn describe_changes(base: &WorldLayouts, other: &WorldLayouts, changed: &[String]) -> String {
    let mut out = format!(
        "roads: {} tracks / {} nodes -> {} tracks / {} nodes (digest {} -> {})\n",
        base.roads.tracks,
        base.roads.nodes,
        other.roads.tracks,
        other.roads.nodes,
        base.roads.digest,
        other.roads.digest
    );
    for (key, before) in &base.roads.per_track {
        match other.roads.per_track.get(key) {
            Some(after) if after == before => {},
            after => {
                let _ = writeln!(out, "  track {key}: {before} -> {after:?} nodes");
            },
        }
    }
    for key in other.roads.per_track.keys() {
        if !base.roads.per_track.contains_key(key) {
            let _ = writeln!(out, "  new track {key}");
        }
    }
    let _ = writeln!(out, "{} site(s) changed layout:", changed.len());
    for key in changed {
        let fmt = |l: Option<&SiteLayout>| {
            l.map_or("-".to_string(), |l| {
                format!("{} plots/{} buildings [{}]", l.plots, l.buildings, l.name)
            })
        };
        let _ = writeln!(
            out,
            "  {key}: {} -> {}",
            fmt(base.sites.get(key)),
            fmt(other.sites.get(key))
        );
    }
    out
}

fn run_perturbation(perturbation: MapPerturbation) -> (WorldLayouts, Vec<String>, String) {
    let base = base_layouts();
    let (world, index) = generate_cromatolis(Some(perturbation));
    let perturbed = world_layouts(&world, &index);
    drop((world, index));
    let changed = changed_sites(base, &perturbed);
    let report = describe_changes(base, &perturbed, &changed);
    println!("{report}");
    (perturbed, changed, report)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

/// The CI guard: every site's layout matches the committed digest file.
/// A failure lists exactly which sites changed; if the change is intended,
/// re-record (see the module doc) and commit the new file with the edit.
///
/// Also writes a full per-site table (with plot kinds) to the path in
/// `XINDELER_SITE_LAYOUT_TABLE`, if set, for before/after reports.
#[test]
#[ignore]
fn cromatolis_site_layout_digests_match_the_committed_file() {
    let layouts = base_layouts();
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join(DIGEST_FILE);

    if let Ok(table_path) = std::env::var("XINDELER_SITE_LAYOUT_TABLE") {
        let mut table = String::new();
        for (key, site) in &layouts.sites {
            let _ = writeln!(
                table,
                "{key}\t{}\t{}\t{}\t{}\t{}\t{}\t{:?}",
                site.name,
                site.plots,
                site.buildings,
                site.digest,
                site.kinds,
                site.port,
                site.authored_size
            );
        }
        let _ = writeln!(
            table,
            "@roads\t-\t{}\t{}\t{}\t-",
            layouts.roads.tracks, layouts.roads.nodes, layouts.roads.digest
        );
        std::fs::write(table_path, table).unwrap();
    }

    let rendered = render_digest_file(layouts);
    if std::env::var(BLESS_ENV).is_ok_and(|v| v == "1") {
        std::fs::write(&path, &rendered).unwrap();
        return;
    }
    let committed = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("cannot read {}: {e}", path.display()));
    let expected = parse_digest_file(&committed);
    let actual = parse_digest_file(&rendered);

    let keys: BTreeSet<&String> = expected.keys().chain(actual.keys()).collect();
    let mismatches: Vec<String> = keys
        .into_iter()
        .filter(|k| expected.get(*k).map(|e| &e.0) != actual.get(*k).map(|a| &a.0))
        .map(|k| {
            format!(
                "  {k}: committed {:?} -> now {:?}",
                expected.get(k),
                actual.get(k)
            )
        })
        .collect();
    assert!(
        mismatches.is_empty(),
        "{} Cromatolis site layout(s) differ from {DIGEST_FILE}:\n{}\nIf this is an intended \
         re-roll (reviewed terrain edit or engine change), re-record with {BLESS_ENV}=1.",
        mismatches.len(),
        mismatches.join("\n")
    );
}

/// Generating the same world twice gives the same layout for every site
/// and the same road network.
#[test]
#[ignore]
fn cromatolis_site_layouts_are_deterministic() {
    // Three generations in one process (std `HashMap`/`HashSet` instances get
    // fresh random keys each time, so an iteration-order dependency shows up
    // here); the digest test against the committed file covers separate
    // processes.
    for _ in 0..2 {
        let (world, index) = generate_cromatolis(None);
        let again = world_layouts(&world, &index);
        drop((world, index));
        assert_eq!(changed_sites(base_layouts(), &again), Vec::<String>::new());
        assert_eq!(base_layouts().roads, again.roads);
    }
}

const ENFORCE_SIZE_BANDS_ENV: &str = "XINDELER_ENFORCE_SIZE_BANDS";

/// Soft check: every authored settlement reaches the building band of its
/// authored size (`civ::seeds::MIN_BUILDINGS_BY_SIZE`). Generation re-draws
/// a starved first layout up to `MAX_LAYOUT_RETRIES` times and otherwise
/// keeps its largest attempt with a warning, so a violation here means even
/// that ran out. Reports `WARN size band` lines; fails only with
/// `XINDELER_ENFORCE_SIZE_BANDS=1`, because what to do about such a town (a
/// larger authored footprint, a terrain edit) is a content decision.
#[test]
#[ignore]
fn authored_settlement_size_bands_soft_check() {
    let mut violations = Vec::new();
    for (key, site) in &base_layouts().sites {
        let Some((category, size)) = site.authored_size else {
            continue;
        };
        let Some(min) = crate::civ::seeds::min_buildings_for(category, size) else {
            continue;
        };
        if site.buildings < min {
            violations.push(format!(
                "{key} ({category}, {size}): {} buildings < {min}",
                site.buildings
            ));
        }
    }
    for v in &violations {
        println!("WARN size band: {v}");
    }
    if std::env::var(ENFORCE_SIZE_BANDS_ENV).is_ok_and(|v| v == "1") {
        assert!(violations.is_empty(), "{}", violations.join("\n"));
    }
}

/// One far-away chunk (the map corner, nowhere near a road or a site) one
/// block higher re-rolls no site at all.
#[test]
#[ignore]
fn raising_one_far_chunk_one_block_rerolls_no_site() {
    let (_, changed, report) = run_perturbation(raise_chunks(vec![Vec2::new(1023, 1023)], 1.0));
    assert!(changed.is_empty(), "{report}");
}

/// A chunk that a procedural connector runs through (between Itos Village
/// and its anchor), raised 5 m: the A* reroutes that connector, and no site
/// re-rolls.
#[test]
#[ignore]
fn raising_a_chunk_on_a_procedural_road_five_metres_rerolls_no_site() {
    let on_road = Vec2::new(122, 219);
    assert!(
        base_layouts()
            .roads
            .paths
            .iter()
            .any(|path| path.contains(&(on_road.x, on_road.y))),
        "premise: {on_road:?} must lie on a road"
    );
    let (_, changed, report) = run_perturbation(raise_chunks(vec![on_road], 5.0));
    assert!(changed.is_empty(), "{report}");
}

/// The whole map raised by the mean quantisation bias of a heightmap
/// re-encode (+1.13 cm). Roads are unchanged (only height *differences*
/// steer them), so this isolates a town generator's sensitivity to its own
/// absolute tile altitudes. Before market stands got their own sub-RNG in
/// authored regions (`plot::plaza`), 8-9 towns re-rolled wholesale here:
/// one stand's `is_even` truncation flipping changed how many numbers the
/// stand loop drew from the town's RNG.
///
/// What is left is local and small -- a plot or two moving inside a town --
/// from the generator's other altitude-dependent steps: retry loops such as
/// `generate_farm`'s `find_rural_aabr` attempts, and street A* costs. Which
/// towns show it depends on the roll, so this is a ratchet on the measured
/// count (seed 0: Evercross moves one house, Neoland one road plot), not a
/// zero guarantee. It may go down; raising it needs review.
#[test]
#[ignore]
fn raising_the_whole_map_by_a_re_encode_bias_stays_local() {
    const MAX_CHANGED_SITES: usize = 2;
    let (perturbed, changed, report) = run_perturbation(raise_everything(0.0113));
    assert_eq!(
        perturbed.roads,
        base_layouts().roads,
        "a uniform shift must not move roads"
    );
    assert!(changed.len() <= MAX_CHANGED_SITES, "{report}");
}

/// A 9-chunk, 40 m wall across the procedural connector at (122, 219)
/// forces a connector one node longer -- the case that used to re-roll every
/// town in the world (53 sites: each site took its seed from the stream the
/// carver had just drawn a different number of offsets from). Now the road
/// re-draws only its own chunk offsets, from its own RNG.
///
/// Not every remaining change is beside the road: raising any chunk also
/// nudges `SimChunk::alt` map-wide by ~1e-5 blocks (the rank-based
/// humidity/temperature terms of the soil warp see the new altitude
/// distribution), which can tip a far town's local altitude threshold.
/// That coupling is terrain, not RNG, so this pins the count, not locality.
#[test]
#[ignore]
fn lengthening_one_procedural_road_does_not_cascade() {
    const MAX_CHANGED_SITES: usize = 3;
    let wall = (215..=223).map(|y| Vec2::new(122, y)).collect();
    let (perturbed, changed, report) = run_perturbation(raise_chunks(wall, 40.0));
    assert_ne!(
        perturbed.roads.nodes,
        base_layouts().roads.nodes,
        "premise: the wall must change a road's node count\n{report}"
    );
    assert!(changed.len() <= MAX_CHANGED_SITES, "{report}");
}

/// Ad-hoc experiment for authoring: how many sites would this edit
/// re-roll? `XINDELER_SITE_LAYOUT_PERTURB` is `;`-separated `x,y,dz`
/// (chunk, blocks) or `all,dz`. Prints the report; never fails on it.
#[test]
#[ignore]
fn perturbation_experiment_from_env() {
    let Ok(spec) = std::env::var("XINDELER_SITE_LAYOUT_PERTURB") else {
        return;
    };
    let mut chunks: Vec<(Vec2<i32>, f64)> = Vec::new();
    let mut everything = 0.0;
    for part in spec.split(';').filter(|p| !p.is_empty()) {
        let cols: Vec<&str> = part.split(',').collect();
        match cols.as_slice() {
            ["all", dz] => everything += dz.parse::<f64>().unwrap(),
            [x, y, dz] => chunks.push((
                Vec2::new(x.parse().unwrap(), y.parse().unwrap()),
                dz.parse().unwrap(),
            )),
            _ => panic!("bad perturbation {part:?}"),
        }
    }
    let perturbation: MapPerturbation = Arc::new(move |size, alt, basement| {
        for (chunk, dz) in &chunks {
            let i = vec2_as_uniform_idx(size, *chunk);
            alt[i] += dz;
            basement[i] += dz;
        }
        alt.iter_mut().for_each(|a| *a += everything);
        basement.iter_mut().for_each(|b| *b += everything);
    });
    let _ = run_perturbation(perturbation);
}

/// Bit-identity guard for every non-Cromatolis world: a digest of the
/// civ layer (every site's stable key and layout, every road) of the
/// default procedural map at seed 0, committed before the Cromatolis-only
/// seed derivation existed. Any change here means non-authored worlds
/// changed, which the Cromatolis seed work must never do.
#[test]
#[ignore]
fn procedural_world_civ_layer_matches_the_committed_digest() {
    let (world, index) = generate_world(crate::sim::DEFAULT_WORLD_MAP, None);
    assert!(
        world.civs.sites.values().all(|s| s.authored_id().is_none()),
        "premise: the default map has no authored sites"
    );
    let layouts = world_layouts(&world, &index);
    drop((world, index));
    let rendered = render_digest_file(&layouts);
    let summary = format!(
        "{}\t{}\t{}\t{}\n",
        sha16(&rendered),
        layouts.sites.len(),
        layouts.roads.tracks,
        layouts.roads.nodes
    );
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join(PROCEDURAL_DIGEST_FILE);
    let header = "# Digest of the civ layer (sites + roads) of the default procedural map, world \
                  seed 0.\n# Checked by \
                  site_layouts::procedural_world_civ_layer_matches_the_committed_digest.\n# \
                  digest\tsites\ttracks\tnodes\n";
    if std::env::var(BLESS_ENV).is_ok_and(|v| v == "1") {
        std::fs::write(&path, format!("{header}{summary}")).unwrap();
        return;
    }
    let committed = std::fs::read_to_string(&path).unwrap();
    let committed = committed
        .lines()
        .find(|l| !l.starts_with('#'))
        .unwrap_or_default();
    assert_eq!(committed, summary.trim_end());
}
