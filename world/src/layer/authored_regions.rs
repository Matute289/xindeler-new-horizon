//! The one site that knows *which* authored layers a world has, and registers
//! their geometry with the engine-general protection index.
//!
//! [`crate::layer::authored_voids`] deliberately imports no authored layer:
//! it owns the shapes, the policy, the query and the margin, and nothing about
//! which regions exist. The authored layers themselves expose their resolved
//! geometry as [`DiscShape`]/[`CapsuleShape`] and know nothing about the index.
//! This module is the seam between the two, and it is the only file a new
//! authored region has to touch to be protected.
//!
//! Keeping the enumeration here rather than inside the index is what makes the
//! claim "a future authored map inherits everything" true: the dependency runs
//! authored layer → index and registration → both, never index → authored
//! layer.

use crate::{
    CanvasInfo,
    index::Index,
    layer::{
        authored_voids::{AuthoredVoids, AuthoredVoidsBuilder},
        cromatolis_cave_features, cromatolis_interior,
        traversal::AccommodationTier,
    },
};

/// This world's authored-void index, building and caching it on first use.
///
/// `None` for a world with no authored region -- the purely-procedural case, in
/// which every consumer short-circuits and generation is bit-identical to
/// having no guard in the tree at all.
pub(crate) fn authored_voids<'a>(info: &CanvasInfo<'a>) -> Option<&'a AuthoredVoids> {
    let index: &'a Index = info.index().index;
    index.authored_voids.get_or_init(|| build(info)).as_ref()
}

/// Reads the already-cached authored geometry on [`Index`]; it never loads or
/// re-parses an asset of its own.
fn build(info: &CanvasInfo) -> Option<AuthoredVoids> {
    // The gate for "an authored region is loaded at all". Deliberately a
    // world-level question, not the per-chunk authored-region flag: the index
    // is world-global, and a tunnel in an unauthored chunk can still run into
    // an authored void a chunk over.
    info.chunks().authored_procedural_layers()?;

    let index = info.index().index;
    let mut builder = AuthoredVoidsBuilder::default();

    for cave in index
        .cromatolis_cave_features
        .get_or_init(|| cromatolis_cave_features::build_all_generated_caves(info))
    {
        // Every shape of one authored cave takes that cave's own authored
        // policy. The index supports splitting it per shape; no authored map
        // needs that yet.
        let contact = cave.procedural_contact();
        // Catalog-parameterised: a human chose the anchor, the size class and
        // the contents, and the shape was derived from those. Air may be
        // opened inside one, but its floor may never be dug -- content is
        // placed on that floor at carve time.
        let tier = AccommodationTier::Catalog;
        builder.push_disc(cave.hub_void_disc(), contact, tier);
        for capsule in cave.branch_void_capsules() {
            builder.push_capsule(capsule, contact, tier);
        }
    }

    for interior in index
        .cromatolis_interiors
        .get_or_init(|| cromatolis_interior::build_all_layouts(info))
    {
        // Interiors carry their policy with their shapes rather than having it
        // stamped on here, because their shape accessors deliberately
        // *over*-approximate the carved volume -- which is only sound for a
        // dilated (`Seal`) shape. See the accessors' own docs.
        // Hand-authored architecture: named rooms, gates, authored surface
        // accesses, and a navigation graph derived from the authored splines.
        // Never edited by anything hash-placed; the intruder is kept out
        // instead.
        let tier = AccommodationTier::HandAuthored;
        for (disc, contact) in interior.void_discs() {
            builder.push_disc(disc, contact, tier);
        }
        for (capsule, contact) in interior.void_capsules() {
            builder.push_capsule(capsule, contact, tier);
        }
    }

    builder.finish()
}
