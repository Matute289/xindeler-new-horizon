//! The one site that knows *which* authored layers a world has, and registers
//! their geometry with the engine-general protection index.
//!
//! [`crate::layer::authored_voids`] deliberately imports no authored layer:
//! it owns the shapes, the policy, the query and the margin, and nothing about
//! which regions exist. The authored layers themselves expose their resolved
//! geometry as `DiscShape`/`CapsuleShape` and know nothing about the index.
//! This module is the seam between the two, and it is the only file a new
//! authored region has to touch to be protected.
//!
//! Keeping the enumeration here rather than inside the index is what makes the
//! claim "a future authored map inherits everything" true: the dependency runs
//! authored layer → index and registration → both, never index → authored
//! layer.
//!
//! # When the index is built
//!
//! Eagerly, once, at the end of world generation ([`warm`]), and *not* lazily
//! from whichever chunk happens to query it first. The distinction is not
//! cosmetic: the index is read from a per-column query that every generated
//! chunk reaches, including chunks nowhere near an authored region, so a lazy
//! build would put asset loading and the whole region's geometry resolution
//! inside a `OnceLock` that the entire first wave of chunk-generation workers
//! then blocks on instead of stealing work. Building it where world generation
//! is already paying for exactly this kind of work removes the question.
//!
//! The lazy path is still correct and still present -- [`authored_voids`] will
//! build on demand if it was never warmed, which is what keeps every test and
//! any future embedder that skips [`warm`] working.

use crate::{
    CanvasInfo, Land,
    index::Index,
    layer::{
        authored_voids::{AuthoredVoids, AuthoredVoidsBuilder},
        cromatolis_cave_features, cromatolis_interior,
    },
    sim::WorldSim,
};
use tracing::info;

/// This world's authored-void index, building and caching it on first use.
///
/// `None` for a world with no authored region -- the purely-procedural case, in
/// which every consumer short-circuits and generation is bit-identical to
/// having no guard in the tree at all.
pub(crate) fn authored_voids<'a>(index: &'a Index, sim: &WorldSim) -> Option<&'a AuthoredVoids> {
    index
        .authored_voids
        .get_or_init(|| build(index, sim))
        .as_ref()
}

/// [`authored_voids`] for a caller that has a live chunk rather than the world
/// directly -- which is every consumer on the per-column path.
///
/// Exists so those call sites stay a single argument: only [`warm`] needs the
/// two-argument form, and threading it everywhere would churn upstream's
/// `cave.rs` for nothing.
pub(crate) fn authored_voids_for<'a>(info: &CanvasInfo<'a>) -> Option<&'a AuthoredVoids> {
    authored_voids(info.index().index, info.chunks())
}

/// Build the authored-void index now, so no chunk-generation worker ever has
/// to. Idempotent, and a no-op for a world with no authored region.
///
/// Called once from `World::generate`. See the module doc for why this is not
/// left to the first query.
pub(crate) fn warm(index: &Index, sim: &WorldSim) {
    let Some(voids) = authored_voids(index, sim) else {
        return;
    };

    // The one check the index cannot run for itself, because it needs the
    // terrain sampler to know where each shape's surface cap falls. Run here,
    // once, on the same thread that built the index -- never from a chunk
    // worker, where it would be both repeated and untimely. Reporting lives
    // here rather than in the index so that stays a pure query.
    let land = Land::from_sim(sim);
    let inert = voids.inert_connect_features(|wpos| land.get_alt_approx(wpos));
    if !inert.is_empty() {
        // `info!`, not `warn!`, and the level is a deliberate choice. This is a
        // standing fact about the authored content, not a fault: it is expected
        // to be non-empty on any map where a connectable feature happens to sit
        // beside a sealed one, and it will print on every server start for as
        // long as that adjacency exists. A permanently-expected warning is how
        // operators learn to ignore warnings. The regression gate is the test
        // that pins this list by name; this line exists so the fact is
        // discoverable at all, not to demand action.
        info!(
            "{} authored feature(s) are marked Connect but sit entirely inside a sealed feature's \
             protection margin, so no procedural tunnel will ever break into them: {inert:?}. \
             This is the Seal-beats-Connect tie-break working as designed -- a promise outranks a \
             permission -- but it does mean the authored intent has no effect. Moving the feature \
             clear of its sealed neighbour is what would give it one.",
            inert.len()
        );
    }
}

/// Reads the already-cached authored geometry on [`Index`]; it never loads or
/// re-parses an asset of its own.
fn build(index: &Index, sim: &WorldSim) -> Option<AuthoredVoids> {
    // The gate for "an authored region is loaded at all". Deliberately a
    // world-level question, not the per-chunk authored-region flag: the index
    // is world-global, and a tunnel in an unauthored chunk can still run into
    // an authored void a chunk over.
    sim.authored_procedural_layers()?;

    let mut builder = AuthoredVoidsBuilder::default();

    for cave in index
        .cromatolis_cave_features
        .get_or_init(|| cromatolis_cave_features::build_all_generated_caves(sim))
    {
        // Every shape of one authored cave takes that cave's own authored
        // policy. The index supports splitting it per shape; no authored map
        // needs that yet.
        let contact = cave.procedural_contact();
        let id = cave.id();
        builder.push_disc(cave.hub_void_disc(), contact, id);
        for capsule in cave.branch_void_capsules() {
            builder.push_capsule(capsule, contact, id);
        }
    }

    for interior in index
        .cromatolis_interiors
        .get_or_init(|| cromatolis_interior::build_all_layouts_for_map_size(sim.map_size_lg()))
    {
        // Interiors carry their policy with their shapes rather than having it
        // stamped on here, because their shape accessors deliberately
        // *over*-approximate the carved volume -- which is only sound for a
        // dilated (`Seal`) shape. See the accessors' own docs.
        let id = interior.id();
        for (disc, contact) in interior.void_discs() {
            builder.push_disc(disc, contact, id);
        }
        for (capsule, contact) in interior.void_capsules() {
            builder.push_capsule(capsule, contact, id);
        }
        for (disc, contact) in interior.void_waterfall_discs() {
            builder.push_disc(disc, contact, id);
        }
    }

    builder.finish()
}
