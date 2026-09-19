//! A layer-agnostic index of every block volume an *authored* feature carves,
//! plus the policy a purely-procedural layer must obey when it meets one.
//!
//! # Why this exists
//!
//! Authored underground geometry (Cromatolis's generic caves and its two
//! bespoke interiors today; any future authored region tomorrow) and the
//! purely-procedural cave lattice in [`crate::layer::cave`] both write into
//! the same block volumes, and neither can see the other. Without an index
//! like this one, the only way to stop a hash-derived tunnel punching through
//! a hand-authored vault is to switch the whole procedural layer off for the
//! region -- at the cost of its entire procedural underground (cave biomes,
//! cave fauna, and every Iron/Coal/Cobalt/Silver and gem deposit in the game,
//! all of which come from that one layer).
//!
//! `AuthoredVoids` is the narrow alternative: a cheap, exact-shape protection
//! index the procedural layers consult at one choke point.
//!
//! # What is engine-general here, and what is not
//!
//! **Everything in this module is engine-general**, and structurally so: it
//! imports no `cromatolis_*` module, and nothing here knows which authored
//! regions exist. Which layers of which region contribute shapes is decided in
//! [`crate::layer::authored_regions`], the one content-aware registration site;
//! the authored layers themselves are thin adapters that hand their
//! already-resolved geometry over as [`DiscShape`]/[`CapsuleShape`]; and the
//! *values* of [`ProceduralContact`] are authored content living in an asset.
//!
//! A future authored map inherits the index, the choke point, the tie-break,
//! the margin and the tests; all it has to do is expose its shapes and register
//! them. If it omits the policy entirely every one of its voids seals --
//! correct by default, never silently exposed.
//!
//! # The two properties that matter
//!
//! * **Exact shape, not a bound.** Each shape mirrors the carve predicate of
//!   the thing it protects (a disc for a chamber/room, a *bowed spline* capsule
//!   for a tunnel/connection), so the protected volume is the carved volume
//!   dilated by one margin -- not a bounding circle several times too big,
//!   which would suppress far more procedural cave than it protects.
//! * **The bowed spline is the primitive.** [`VoidGeom::Capsule`] stores
//!   `curve` and is evaluated with the same quadratic spline the carve uses,
//!   never the straight chord between its endpoints. Real authored tunnels bow
//!   well off their chord, so a chord-based capsule would protect a
//!   substantially different volume from the one actually carved.
//!
//! # Cost
//!
//! `shapes` is on the order of a thousand entries for a whole authored region.
//! A chunk-granular bucket grid over them means the common case -- a chunk
//! nowhere near an authored feature -- is a single hash lookup, after which
//! the guard is a slice-is-empty test per column rather than a scan over every
//! shape.

use common::{
    terrain::{TerrainChunkSize, quadratic_nearest_point, river_spline_coeffs},
    vol::RectVolSize,
};
use hashbrown::HashMap;
use serde::Deserialize;
use std::ops::{Range, RangeInclusive};
use vek::*;

/// How far a procedural feature must stay clear of a
/// [`ProceduralContact::Seal`] authored void, in blocks, in every direction
/// (horizontally *and* vertically -- so a tunnel does not merely miss an
/// authored ceiling but leaves a real rock slab above it).
///
/// **This is the `Seal` margin and only the `Seal` margin.**
/// [`ProceduralContact::Connect`] has no margin at all -- not "a smaller
/// margin", and not zero-as-a-tuning-choice. The clip's unit is a *column*, so
/// a margin shell around a `Connect` void would delete the procedural tunnel
/// in an annulus between the outside world and the authored chamber, and the
/// tunnel would then reappear on the far side of that annulus, *inside* the
/// chamber, as an air pocket completely walled off from the tunnel it belongs
/// to. That is worse than either alternative the margin was ever meant to
/// choose between. See [`VoidShape::margin`].
///
/// # Cost curve (measured over the real authored Cromatolis region)
///
/// | margin | authored caves protected | procedural cave volume suppressed |
/// |-------:|-------------------------:|----------------------------------:|
/// |      0 |                       19 |                          0.0058 % |
/// |      8 |                       29 |                          0.0131 % |
/// | **16** |                   **35** |                      **0.0277 %** |
/// |     32 |                       51 |                          0.0875 % |
///
/// 16 was chosen because it catches the near-misses that would otherwise read
/// as a paper-thin wall in game (nearly double margin 0's protected set) while
/// still costing under three hundredths of a percent of the region's
/// procedural cave volume, and because it leaves a rock slab thicker than all
/// but one of the authored headrooms in the catalogue, and thicker than the
/// median carved tunnel half-height. Below 16 the guard stops doing the one
/// thing it is for.
///
/// **That table is an *upper* bound, not necessarily the realised cost.** It
/// was measured with the guard applied to every authored cave. `Connect` voids
/// are exempt from the guard entirely, so the realised suppressed volume is at
/// most this and falls as authored data declares more of them -- by how much is
/// geometry-weighted, not count-weighted, and has to be measured rather than
/// inferred from how many voids take each policy. For an authored asset that
/// declares no `Connect` at all, the realised cost simply *is* the table.
///
/// **One value for every authored region.** If a second region ever wants a
/// different one, this repo already has the right home for a per-region knob --
/// `sim::AuthoredProceduralLayers`, loaded from that region's own
/// `*_procedural_layers.ron` -- and moving it there is a field plus a lookup,
/// not a redesign. It is a code constant today because there is exactly one
/// authored region and no second opinion about the number.
pub(crate) const AUTHORED_VOID_MARGIN: f32 = 16.0;

/// How far past its nominal radius an authored carve's own edge test reaches.
///
/// Every authored carve site uses it the same way (`edge_weight(dist, radius)`,
/// plus a slightly generous cheap-reject at `radius + EDGE_SOFTNESS`).
///
/// Defined **here**, and read by the carve sites, rather than the other way
/// round: the protection index is *defined* as the carved volume dilated by one
/// margin, so a one-sided edit to a private copy would silently break that
/// equality with nothing but an `#[ignore]`d real-asset test to catch it. From
/// this module's point of view the slack is simply more protection, so it is
/// folded into the `Seal` dilation -- see [`VoidShape::margin`].
pub(crate) const EDGE_SOFTNESS: f32 = 3.0;

/// How far below the terrain surface an authored ceiling is always capped, so a
/// shallow authored void can never punch a hole to the sky.
///
/// Defined here and read by the carve sites for the same reason as
/// [`EDGE_SOFTNESS`]. It is load-bearing for the index: the protected band is
/// derived from the *capped* ceiling the carve actually produces, and only then
/// dilated by the margin (see [`protected_band`]).
pub(crate) const SURFACE_MARGIN: f32 = 4.0;

/// What a purely-procedural layer must do when it meets an authored void.
///
/// This is the engine-side form of the authored per-feature "may a procedural
/// tunnel cross into this?" decision. It is a named enum rather than a `bool`
/// because the two variants are genuinely different code paths at the choke
/// point, because `if !allow_procedural_connection` is the classic
/// inverted-boolean footgun, and because a third mode (say "connect, but dress
/// the mouth") could be added later without changing a single call site's
/// type.
///
/// `Deserialize` because an authored region's own asset is where the *values*
/// come from -- as a bare enum literal, e.g. `procedural_contact: Connect`.
/// Nothing else about this type is content-aware.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Deserialize)]
pub(crate) enum ProceduralContact {
    /// The procedural feature is cut back at this column, leaving solid rock.
    /// The authored void stays exactly as authored. This is the conservative
    /// default: an authored asset that never mentions the field, or a future
    /// map that never thought about it, gets full protection.
    Seal,
    /// The procedural feature passes through unaltered. What the player ends
    /// up seeing is the procedural tunnel's own tapering opening in the
    /// authored wall -- the authored carve runs *after* the procedural one and
    /// unconditionally overwrites its own whole footprint, so no blending or
    /// join code is needed (or wanted) to produce it.
    Connect,
}

/// A disc-shaped authored void: a chamber or a room.
///
/// Mirrors the carve predicate of `cromatolis_cave_features`'s hub chambers
/// and `cromatolis_interior`'s level rooms: a column is inside iff its 2D
/// distance to `centre` is below `radius`, and the carved band is
/// `floor_z ..= min(ceiling_z, col_alt - SURFACE_MARGIN)`.
pub(crate) struct DiscShape {
    pub centre: Vec2<i32>,
    pub radius: f32,
    pub floor_z: i32,
    pub ceiling_z: i32,
}

/// Which end of a [`CapsuleShape`]'s vertical extent its endpoint `z` anchors,
/// and therefore which end the surface cap moves.
///
/// The distinction is not cosmetic. An air passage is anchored at its floor and
/// the cap lowers only its ceiling; a body of water is anchored at its surface
/// and the cap lowers the *whole* band, floor included. Modelling water the
/// first way leaves the protected floor above the real carved bottom wherever
/// the cap binds, which under-protects exactly where the terrain is shallowest.
#[derive(Clone, Copy)]
pub(crate) enum CapsuleSpan {
    /// The endpoint `z` is the carved **floor**; the void rises `headroom`
    /// above it, capped at the surface. Air passages: branch tunnels, interior
    /// connections.
    AboveFloor { headroom: f32 },
    /// The endpoint `z` is the carved **surface**; the void hangs `depth` below
    /// that surface *after* it has been capped, so the cap lowers floor and
    /// ceiling together. Liquid bodies.
    BelowSurface { depth: f32 },
}

/// A capsule-shaped authored void following a *bowed* quadratic spline: a
/// branch tunnel, an interior connection, or a water feature.
///
/// `a`/`b` are the endpoints (their `z` meaning is set by `span`), `r_a`/`r_b`
/// the radii at each end (lerped along the curve), and `curve` the same bow
/// parameter the carve passes to its spline.
///
/// The end caps are rounded, not flat: the spline solver clamps its parameter
/// into `[0, 1]`, so a column past an endpoint measures its distance to that
/// endpoint. The carve behaves identically, which is the point -- and it means
/// a `Seal` shape's margin really does apply in every direction, ends included.
pub(crate) struct CapsuleShape {
    pub a: Vec3<i32>,
    pub b: Vec3<i32>,
    pub r_a: f32,
    pub r_b: f32,
    pub curve: f32,
    pub span: CapsuleSpan,
}

enum VoidGeom {
    Disc {
        centre: Vec2<i32>,
        radius: f32,
        floor_z: i32,
        ceiling_z: i32,
    },
    Capsule {
        a: Vec3<i32>,
        b: Vec3<i32>,
        r_a: f32,
        r_b: f32,
        curve: f32,
        span: CapsuleSpan,
    },
}

struct VoidShape {
    geom: VoidGeom,
    /// This shape's *dilated* 2D bounding box, precomputed at build time.
    ///
    /// Purely an optimisation, and the only reason it is stored rather than
    /// recomputed: the bucket grid is chunk-granular over an AABB, so a column
    /// merely inside a bowed capsule's bounding box -- not under the capsule --
    /// still finds that shape in its bucket. Without this reject such a column
    /// pays a full cubic spline solve per shape, per column, and that is
    /// exactly the cost profile of a chunk *inside* an authored cave's
    /// footprint, where several branch capsules share the bucket. Four integer
    /// compares turn every AABB false positive into a no-op.
    aabr: Aabr<i32>,
    /// What a procedural layer must do when it meets **this** shape. Set from
    /// the authored feature the shape came from.
    ///
    /// It lives per-shape rather than per-feature on purpose: a consumer never
    /// has to know whether a shape came from a cave, an interior room or some
    /// future authored layer, and a future map can give one feature's chambers
    /// one policy and its outlying galleries another without a type change.
    /// Today every shape of one authored cave carries that cave's single
    /// authored value -- the per-shape split is capability, not a decision
    /// anyone has to make now.
    contact: ProceduralContact,
}

impl VoidShape {
    /// How far this shape's protected volume is dilated past the volume the
    /// carve actually produces.
    ///
    /// `Seal` gets [`AUTHORED_VOID_MARGIN`] plus the carve's own
    /// [`EDGE_SOFTNESS`] slack. **`Connect` gets exactly zero**, structurally
    /// rather than by choice: any nonzero dilation of a `Connect` shape
    /// deletes a ring of procedural tunnel between the outside world and the
    /// authored chamber and strands the rest of that tunnel as a sealed air
    /// pocket inside it. A `Connect` shape is therefore tested against its
    /// *exact* carved volume. See [`AUTHORED_VOID_MARGIN`].
    ///
    /// The cost of a zero margin on `Connect` is that a tunnel which
    /// near-misses by a block or two leaves a paper-thin membrane. Under
    /// `Seal` that would be a defect; under `Connect` it is not, because the
    /// author has already agreed this void may be breached, so "a thin wall
    /// the player can mine" and "a connection" are both acceptable ends of the
    /// same intent.
    fn margin(&self) -> f32 { margin_for(self.contact) }
}

/// See [`VoidShape::margin`] -- split out so the build site can size a shape's
/// bounding box before the shape exists.
fn margin_for(contact: ProceduralContact) -> f32 {
    match contact {
        ProceduralContact::Seal => AUTHORED_VOID_MARGIN + EDGE_SOFTNESS,
        ProceduralContact::Connect => 0.0,
    }
}

/// The shared authored-void protection index for one world.
///
/// Built once (cached on [`Index`], see [`authored_voids`]) and thereafter
/// read-only.
pub(crate) struct AuthoredVoids {
    shapes: Vec<VoidShape>,
    /// Chunk-granular bucket grid over `shapes`, keyed by chunk position, so
    /// the per-chunk lookup is one hash instead of a scan over every shape.
    /// Buckets are `Vec<u32>` (indices into `shapes`) rather than inline
    /// storage because the grid is built once and the overwhelmingly common
    /// case is an *absent* key, which allocates nothing at all.
    grid: HashMap<Vec2<i32>, Vec<u32>>,
}

/// The authored-void shapes that could claim any column of one chunk.
///
/// Resolve this once per column (or once per chunk) and reuse it across every
/// candidate procedural feature at that column: it turns the guard's cost from
/// "one hash per query" into "one hash, then a slice test".
#[derive(Clone, Copy)]
pub(crate) struct ChunkVoids<'a> {
    voids: &'a AuthoredVoids,
    shapes: &'a [u32],
}

impl ChunkVoids<'_> {
    /// Whether no authored shape can reach any column of this chunk. When
    /// true, [`Self::contact_at_column`] is guaranteed to return `None` and
    /// can be skipped entirely.
    pub(crate) fn is_empty(&self) -> bool { self.shapes.is_empty() }

    /// The policy that applies at `wpos2d` over the block range `z`, or `None`
    /// when no authored shape claims this column at this z-band.
    ///
    /// When several shapes claim it, the **strongest** policy wins:
    /// [`ProceduralContact::Seal`] beats [`ProceduralContact::Connect`]. That
    /// tie-break is deliberate -- `Seal` is a promise ("this authored volume
    /// is never breached") and `Connect` is only a permission, and a
    /// permission must never override a promise. So a column claimed by both a
    /// `Connect` cave and a neighbouring `Seal` cave's protection shell seals.
    ///
    /// Note that the only production consumer today acts on `Seal` and treats
    /// `Connect` and `None` alike -- `Connect`'s whole behaviour *is* doing
    /// nothing. The distinction is still load-bearing, because it is what the
    /// authored data says and what a policy-aware consumer added later reads;
    /// but nothing downstream observes it yet.
    pub(crate) fn contact_at_column(
        &self,
        wpos2d: Vec2<i32>,
        col_alt: f32,
        z: &Range<i32>,
    ) -> Option<ProceduralContact> {
        let mut found = None;
        for &idx in self.shapes {
            let shape = &self.voids.shapes[idx as usize];
            // A second `Connect` can tell us nothing a first one has not, and
            // `Seal` wins outright, so only an unmatched-so-far `Connect` or
            // any `Seal` is worth the geometry.
            if found.is_some() && shape.contact == ProceduralContact::Connect {
                continue;
            }
            // Four integer compares before any spline solve. The bucket is
            // chunk-granular over a bounding box, so most shapes it hands back
            // for a given column are not actually over that column.
            if !aabr_contains(&shape.aabr, wpos2d) {
                continue;
            }
            let Some(band) = protected_band(shape, wpos2d, col_alt) else {
                continue;
            };
            if !band_overlaps(&band, z) {
                continue;
            }
            match shape.contact {
                ProceduralContact::Seal => return Some(ProceduralContact::Seal),
                ProceduralContact::Connect => found = Some(ProceduralContact::Connect),
            }
        }
        found
    }
}

/// Collects the shapes of one world's authored layers into an
/// [`AuthoredVoids`].
///
/// The registration site ([`crate::layer::authored_regions`]) drives this: it
/// is what knows which authored layers exist, so that knowledge never leaks
/// into the index itself.
#[derive(Default)]
pub(crate) struct AuthoredVoidsBuilder {
    shapes: Vec<VoidShape>,
}

impl AuthoredVoidsBuilder {
    pub(crate) fn push_disc(&mut self, disc: DiscShape, contact: ProceduralContact) {
        self.push(
            VoidGeom::Disc {
                centre: disc.centre,
                radius: disc.radius,
                floor_z: disc.floor_z,
                ceiling_z: disc.ceiling_z,
            },
            contact,
        );
    }

    pub(crate) fn push_capsule(&mut self, capsule: CapsuleShape, contact: ProceduralContact) {
        self.push(
            VoidGeom::Capsule {
                a: capsule.a,
                b: capsule.b,
                r_a: capsule.r_a,
                r_b: capsule.r_b,
                curve: capsule.curve,
                span: capsule.span,
            },
            contact,
        );
    }

    fn push(&mut self, geom: VoidGeom, contact: ProceduralContact) {
        let aabr = shape_aabr(&geom, margin_for(contact));
        self.shapes.push(VoidShape {
            geom,
            aabr,
            contact,
        });
    }

    /// The finished index, or `None` when nothing was registered -- which is
    /// what makes every consumer a no-op, and generation bit-identical, for a
    /// world with no authored region.
    pub(crate) fn finish(self) -> Option<AuthoredVoids> {
        (!self.shapes.is_empty()).then(|| AuthoredVoids::from_shapes(self.shapes))
    }
}

impl AuthoredVoids {
    fn from_shapes(shapes: Vec<VoidShape>) -> Self {
        let mut grid: HashMap<Vec2<i32>, Vec<u32>> = HashMap::new();
        for (idx, shape) in shapes.iter().enumerate() {
            let min = wpos_to_cpos(shape.aabr.min);
            let max = wpos_to_cpos(shape.aabr.max);
            for y in min.y..=max.y {
                for x in min.x..=max.x {
                    grid.entry(Vec2::new(x, y)).or_default().push(idx as u32);
                }
            }
        }
        Self { shapes, grid }
    }

    /// The shapes that could claim any column of the chunk containing
    /// `wpos2d`. One hash lookup; see [`ChunkVoids`].
    pub(crate) fn in_chunk(&self, wpos2d: Vec2<i32>) -> ChunkVoids<'_> {
        ChunkVoids {
            voids: self,
            shapes: self
                .grid
                .get(&wpos_to_cpos(wpos2d))
                .map_or(&[][..], |bucket| bucket.as_slice()),
        }
    }

    /// One-shot form of [`ChunkVoids::contact_at_column`], for callers that
    /// query a single column and have nothing to amortise the bucket lookup
    /// over.
    ///
    /// Test-gated today: every production consumer reads many candidate
    /// features at one column, so it resolves [`Self::in_chunk`] once and
    /// reuses it. Widening this is a one-line attribute change.
    #[cfg(test)]
    pub(crate) fn contact_at_column(
        &self,
        wpos2d: Vec2<i32>,
        col_alt: f32,
        z: &Range<i32>,
    ) -> Option<ProceduralContact> {
        self.in_chunk(wpos2d).contact_at_column(wpos2d, col_alt, z)
    }

    /// Whether *any* authored shape claims this column at this z-band,
    /// regardless of its policy.
    ///
    /// Deliberately policy-blind, for consumers whose concern is physical
    /// obstruction rather than whether a breach is allowed -- a boulder
    /// dropped into a `Connect` cave is just as stuck as one dropped into a
    /// `Seal` cave, so a guard that keeps surface rocks out of authored
    /// volumes must not consult [`ProceduralContact`].
    ///
    /// Test-gated only because no obstruction-aware consumer exists yet;
    /// widening it is a one-line attribute change, not a redesign.
    #[cfg(test)]
    pub(crate) fn intersects_column(
        &self,
        wpos2d: Vec2<i32>,
        col_alt: f32,
        z: &Range<i32>,
    ) -> bool {
        self.contact_at_column(wpos2d, col_alt, z).is_some()
    }

    /// Number of indexed shapes. Tests only.
    #[cfg(test)]
    pub(crate) fn len(&self) -> usize { self.shapes.len() }

    /// Every shape's **carved** (undilated) z-band at this column, paired with
    /// that shape's policy.
    ///
    /// This is the volume the authored carve actually produces, not the
    /// dilated protection shell -- so it is what an overlap measurement must
    /// compare a procedural feature's z-range against. Tests only.
    #[cfg(test)]
    pub(crate) fn carved_bands_at_column(
        &self,
        wpos2d: Vec2<i32>,
        col_alt: f32,
    ) -> Vec<(RangeInclusive<f32>, ProceduralContact)> {
        self.in_chunk(wpos2d)
            .shapes
            .iter()
            .filter_map(|&idx| {
                let shape = &self.shapes[idx as usize];
                band_dilated_by(&shape.geom, 0.0, wpos2d, col_alt).map(|band| (band, shape.contact))
            })
            .collect()
    }

    /// `(chunk buckets, total bucket entries, largest bucket)`.
    ///
    /// The per-column guard is only cheap while a bucket holds a handful of
    /// shapes, so this turns "the grid is small" from a claim into a number a
    /// test can assert on. Tests only.
    #[cfg(test)]
    pub(crate) fn grid_stats(&self) -> (usize, usize, usize) {
        (
            self.grid.len(),
            self.grid.values().map(Vec::len).sum(),
            self.grid.values().map(Vec::len).max().unwrap_or(0),
        )
    }

    /// How many indexed shapes carry each policy, as `(seal, connect)`. Tests
    /// only.
    #[cfg(test)]
    pub(crate) fn policy_counts(&self) -> (usize, usize) {
        self.shapes
            .iter()
            .fold((0, 0), |(seal, connect), shape| match shape.contact {
                ProceduralContact::Seal => (seal + 1, connect),
                ProceduralContact::Connect => (seal, connect + 1),
            })
    }
}

/// The protected z-band this shape claims at this column, already dilated by
/// the shape's own margin -- or `None` when the shape does not reach this
/// column, or carves nothing here.
///
/// Two details are load-bearing:
///
/// * The ceiling is clamped to `col_alt - SURFACE_MARGIN` **before** the margin
///   is added, because that is the ceiling the carve actually produces. Adding
///   the margin first would let a 16-block dilation over a void whose ceiling
///   already sits 4 blocks below ground reach *above* ground, and start eating
///   surface-level tunnel mouths.
/// * When the clamped ceiling is at or below the floor, the carve produces
///   nothing at this column -- so neither does the protection. Otherwise the
///   guard would plug procedural tunnels under ground the authored feature
///   never actually opened.
fn protected_band(
    shape: &VoidShape,
    wpos2d: Vec2<i32>,
    col_alt: f32,
) -> Option<RangeInclusive<f32>> {
    band_dilated_by(&shape.geom, shape.margin(), wpos2d, col_alt)
}

/// [`protected_band`]'s core, with the dilation supplied rather than derived,
/// so the *carved* (undilated) volume can also be asked for -- which is what
/// an overlap measurement has to compare a procedural feature against, rather
/// than the protection shell.
fn band_dilated_by(
    geom: &VoidGeom,
    margin: f32,
    wpos2d: Vec2<i32>,
    col_alt: f32,
) -> Option<RangeInclusive<f32>> {
    match geom {
        VoidGeom::Disc {
            centre,
            radius,
            floor_z,
            ceiling_z,
        } => {
            let dist = wpos2d.map(|e| e as f32).distance(centre.map(|e| e as f32));
            if dist >= *radius + margin {
                return None;
            }
            // Mirrors the hub/room carve, which floors the surface cap before
            // comparing it as an integer z.
            let ceiling = (*ceiling_z as f32).min((col_alt - SURFACE_MARGIN).floor());
            let floor = *floor_z as f32;
            if ceiling <= floor {
                return None;
            }
            Some((floor - margin)..=(ceiling + margin))
        },
        VoidGeom::Capsule {
            a,
            b,
            r_a,
            r_b,
            curve,
            span,
        } => {
            let a2 = a.xy().map(|e| e as f64 + 0.5);
            let b2 = b.xy().map(|e| e as f64 + 0.5);
            let (t, dist) = spline_sample(a2, b2, *curve, wpos2d.map(|e| e as f64 + 0.5))?;
            let radius = Lerp::lerp_unclamped(*r_a as f64, *r_b as f64, t) as f32;
            if dist as f32 >= radius + margin {
                return None;
            }
            let anchor = Lerp::lerp_unclamped(a.z as f64, b.z as f64, t) as f32;
            let cap = col_alt - SURFACE_MARGIN;
            let (floor, ceiling) = match span {
                // Floor-anchored: the cap lowers only the ceiling.
                CapsuleSpan::AboveFloor { headroom } => (anchor, (anchor + headroom).min(cap)),
                // Surface-anchored: the cap lowers the whole band, so the floor
                // moves with it. Modelling this as floor-anchored would leave
                // the protected floor above the real carved bottom wherever the
                // cap binds.
                CapsuleSpan::BelowSurface { depth } => {
                    let top = anchor.min(cap);
                    (top - depth, top)
                },
            };
            if ceiling <= floor {
                return None;
            }
            Some((floor - margin)..=(ceiling + margin))
        },
    }
}

/// Whether a procedural feature's `[start, end)` block range touches a
/// protected band.
fn band_overlaps(band: &RangeInclusive<f32>, z: &Range<i32>) -> bool {
    z.start as f32 <= *band.end() && z.end as f32 >= *band.start()
}

/// Whether a column falls inside a shape's dilated bounding box.
fn aabr_contains(aabr: &Aabr<i32>, wpos2d: Vec2<i32>) -> bool {
    wpos2d.x >= aabr.min.x
        && wpos2d.x <= aabr.max.x
        && wpos2d.y >= aabr.min.y
        && wpos2d.y <= aabr.max.y
}

/// The 2D world-space box this shape's *protected* volume can reach, used both
/// to decide which chunk buckets it belongs in and as the per-column cheap
/// reject. Conservative for a capsule: the convex hull of a quadratic spline is
/// the hull of its three control points, so their bounding box always contains
/// the curve.
///
/// A capsule's box grows with `|b - a| * |curve|`, so a future authored map
/// that exposes `curve` straight from an asset can inflate both the bucket
/// count and the number of per-column false positives. Today's producers derive
/// `curve` in code and bound it well under 0.5, which keeps the worst box to a
/// handful of chunks; anything that starts authoring `curve` should bound it at
/// its own registration site.
fn shape_aabr(geom: &VoidGeom, margin: f32) -> Aabr<i32> {
    match geom {
        VoidGeom::Disc { centre, radius, .. } => {
            let reach = (*radius + margin).ceil() as i32;
            Aabr {
                min: *centre - reach,
                max: *centre + reach,
            }
        },
        VoidGeom::Capsule {
            a,
            b,
            r_a,
            r_b,
            curve,
            ..
        } => {
            let reach = (r_a.max(*r_b) + margin).ceil() as i32;
            let a2 = a.xy().map(|e| e as f64 + 0.5);
            let b2 = b.xy().map(|e| e as f64 + 0.5);
            // The quadratic's Bezier control point: `p(t) = a t^2 + b t + c`
            // with `b` the control offset puts it half an offset off `a2`.
            let ctrl = a2 + spline_ctrl_offset(a2, b2, *curve).map(f64::from) * 0.5;
            let hull = [a2, b2, ctrl];
            let min = hull.iter().fold(Vec2::broadcast(f64::INFINITY), |acc, p| {
                acc.map2(*p, f64::min)
            });
            let max = hull
                .iter()
                .fold(Vec2::broadcast(f64::NEG_INFINITY), |acc, p| {
                    acc.map2(*p, f64::max)
                });
            Aabr {
                min: min.map(|e| e.floor() as i32) - reach,
                max: max.map(|e| e.ceil() as i32) + reach,
            }
        },
    }
}

/// The spline's control offset (its derivative at `t = 0`), shared by
/// [`spline_sample`] and the capsule's bounding box so the two can never
/// disagree about where the curve goes.
fn spline_ctrl_offset(a2: Vec2<f64>, b2: Vec2<f64>, curve: f32) -> Vec2<f32> {
    ((b2 - a2) * 0.5
        + ((b2 - a2) * 0.5).rotated_z(std::f64::consts::FRAC_PI_2) * 6.0 * curve as f64)
        .map(|e| e as f32)
}

/// The quadratic spline a bowed authored tunnel actually follows, sampled at
/// one column: returns `t` (0 at `a2`, 1 at `b2`) and the perpendicular
/// distance from the column to the curve.
///
/// The same technique as the copies in the two Cromatolis carve modules, on
/// purpose: the protection shape has to agree with the carve exactly, and a
/// capsule built on the straight chord between the endpoints would be a
/// different shape entirely wherever `curve` is non-zero. Those two copies are
/// left where they are -- each module documents why it keeps its own -- and
/// this is the engine-general one a future authored layer should reuse.
///
/// `quadratic_nearest_point` clamps its parameter into `[0, 1]` before
/// returning, so a column past an endpoint reports that endpoint and its own
/// distance to it -- i.e. the shape has **rounded end caps**, and a dilated
/// shape's margin applies past its ends as well as along its flanks. The range
/// check below therefore never fires today; it is kept because the two carve
/// copies have it, and dropping it here would be the one edit that could make
/// the protected volume stop matching the carved one.
fn spline_sample(a2: Vec2<f64>, b2: Vec2<f64>, curve: f32, point: Vec2<f64>) -> Option<(f64, f64)> {
    let spline = river_spline_coeffs(a2, spline_ctrl_offset(a2, b2, curve), b2);
    let (t, closest, dist_sq) = quadratic_nearest_point(&spline, point, Vec2::new(a2, b2))?;
    if !(0.0..=1.0).contains(&t) {
        return None;
    }
    Some((t, closest.distance(point).min(dist_sq.sqrt())))
}

/// Chunk position of a world position, at `TerrainChunkSize` granularity.
fn wpos_to_cpos(wpos: Vec2<i32>) -> Vec2<i32> {
    wpos.map2(TerrainChunkSize::RECT_SIZE, |e, sz| e.div_euclid(sz as i32))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A terrain altitude high enough that the surface cap never binds, so a
    /// test that is not *about* the cap can ignore it.
    const HIGH_ABOVE: f32 = 10_000.0;

    /// A shape to register, built through the same builder production uses so
    /// a test can never construct a `VoidShape` production could not.
    enum TestShape {
        Disc(DiscShape, ProceduralContact),
        Capsule(CapsuleShape, ProceduralContact),
    }

    fn disc_at(centre: Vec2<i32>, contact: ProceduralContact) -> TestShape {
        TestShape::Disc(
            DiscShape {
                centre,
                radius: 20.0,
                floor_z: 0,
                ceiling_z: 12,
            },
            contact,
        )
    }

    fn disc(contact: ProceduralContact) -> TestShape { disc_at(Vec2::new(0, 0), contact) }

    fn capsule(curve: f32, contact: ProceduralContact) -> TestShape {
        TestShape::Capsule(
            CapsuleShape {
                a: Vec3::new(0, 0, 0),
                b: Vec3::new(200, 0, 0),
                r_a: 10.0,
                r_b: 10.0,
                curve,
                span: CapsuleSpan::AboveFloor { headroom: 8.0 },
            },
            contact,
        )
    }

    fn voids(shapes: Vec<TestShape>) -> AuthoredVoids {
        let mut builder = AuthoredVoidsBuilder::default();
        for shape in shapes {
            match shape {
                TestShape::Disc(disc, contact) => builder.push_disc(disc, contact),
                TestShape::Capsule(capsule, contact) => builder.push_capsule(capsule, contact),
            }
        }
        builder.finish().expect("the fixture registered no shapes")
    }

    #[test]
    fn a_column_inside_a_seal_disc_seals() {
        let v = voids(vec![disc(ProceduralContact::Seal)]);
        assert_eq!(
            v.contact_at_column(Vec2::new(5, 0), HIGH_ABOVE, &(0..12)),
            Some(ProceduralContact::Seal)
        );
    }

    /// The whole point of the margin: a tunnel that merely *near-misses* a
    /// sealed authored void is still cut back, so a real rock slab survives
    /// between the two rather than a paper-thin membrane.
    #[test]
    fn a_seal_disc_protects_out_to_the_full_margin() {
        let v = voids(vec![disc(ProceduralContact::Seal)]);
        // radius 20 + margin 16 + edge softness 3 = 39.
        assert_eq!(
            v.contact_at_column(Vec2::new(38, 0), HIGH_ABOVE, &(0..12)),
            Some(ProceduralContact::Seal),
            "just inside the dilated radius must still seal"
        );
        assert_eq!(
            v.contact_at_column(Vec2::new(40, 0), HIGH_ABOVE, &(0..12)),
            None,
            "past the dilated radius the authored void claims nothing"
        );
    }

    /// The `Connect` half of the margin rule, and the one that would be a real
    /// in-game artifact if it regressed: a `Connect` shape must be tested
    /// against its *exact* carved volume, with no halo at all. A halo would
    /// delete a ring of procedural tunnel outside the chamber and strand the
    /// rest of it as a walled-off air pocket inside.
    #[test]
    fn a_connect_disc_has_no_margin_at_all() {
        let v = voids(vec![disc(ProceduralContact::Connect)]);
        assert_eq!(
            v.contact_at_column(Vec2::new(19, 0), HIGH_ABOVE, &(0..12)),
            Some(ProceduralContact::Connect),
            "the last column inside the carved radius is claimed"
        );
        assert_eq!(
            v.contact_at_column(Vec2::new(20, 0), HIGH_ABOVE, &(0..12)),
            None,
            "one block outside the carved radius, a Connect shape claims nothing"
        );
    }

    /// `Seal` is a promise, `Connect` only a permission, so a column claimed
    /// by both must seal -- whichever order the shapes happen to be indexed
    /// in.
    #[test]
    fn seal_beats_connect_where_both_claim_a_column() {
        // Column 10 is inside the Connect disc (radius 20 at the origin) and
        // inside the Seal disc's 39-block protection shell (45 - 39 = 6).
        let query = |v: &AuthoredVoids| v.contact_at_column(Vec2::new(10, 0), HIGH_ABOVE, &(0..12));
        let connect = disc(ProceduralContact::Connect);
        let seal = disc_at(Vec2::new(45, 0), ProceduralContact::Seal);
        assert_eq!(
            query(&voids(vec![connect, seal])),
            Some(ProceduralContact::Seal)
        );
        let connect = disc(ProceduralContact::Connect);
        let seal = disc_at(Vec2::new(45, 0), ProceduralContact::Seal);
        assert_eq!(
            query(&voids(vec![seal, connect])),
            Some(ProceduralContact::Seal)
        );
    }

    #[test]
    fn a_z_band_above_or_below_the_protected_volume_claims_nothing() {
        let v = voids(vec![disc(ProceduralContact::Seal)]);
        assert_eq!(
            v.contact_at_column(Vec2::new(0, 0), HIGH_ABOVE, &(100..120)),
            None,
            "well above the dilated ceiling"
        );
        assert_eq!(
            v.contact_at_column(Vec2::new(0, 0), HIGH_ABOVE, &(-200..-100)),
            None,
            "well below the dilated floor"
        );
        assert_eq!(
            v.contact_at_column(Vec2::new(0, 0), HIGH_ABOVE, &(20..30)),
            Some(ProceduralContact::Seal),
            "within the vertical margin above the ceiling it is still protected"
        );
    }

    /// The ceiling is clamped to the surface cap *before* the margin is added.
    /// Without that, a 16-block dilation over a void whose ceiling already
    /// sits `SURFACE_MARGIN` below ground would reach above ground and start
    /// suppressing surface-level tunnel mouths.
    #[test]
    fn the_ceiling_is_clamped_to_the_surface_before_the_margin_is_added() {
        let v = voids(vec![disc(ProceduralContact::Seal)]);
        // Terrain 8 blocks up: the carve's ceiling cap is 8 - 4 = 4, well
        // below the shape's own ceiling of 12. The protected band must
        // therefore top out at 4 + 19 = 23, not 12 + 19 = 31.
        let col_alt = 8.0;
        assert_eq!(
            v.contact_at_column(Vec2::new(0, 0), col_alt, &(23..24)),
            Some(ProceduralContact::Seal)
        );
        assert_eq!(
            v.contact_at_column(Vec2::new(0, 0), col_alt, &(25..26)),
            None,
            "the margin must be measured from the CAPPED ceiling, not the authored one"
        );
    }

    /// Where the surface cap leaves no room at all, the carve produces nothing
    /// -- so the guard must protect nothing, rather than plugging tunnels
    /// under ground the authored feature never opened.
    #[test]
    fn a_void_the_surface_cap_squeezes_to_nothing_protects_nothing() {
        let v = voids(vec![disc(ProceduralContact::Seal)]);
        assert_eq!(
            v.contact_at_column(Vec2::new(0, 0), 2.0, &(0..12)),
            None,
            "cap 2 - 4 = -2 is at or below the floor, so no carve and no protection"
        );
    }

    #[test]
    fn a_chunk_with_no_shapes_near_it_is_an_empty_bucket() {
        let v = voids(vec![disc(ProceduralContact::Seal)]);
        let far = Vec2::new(100_000, 100_000);
        assert!(v.in_chunk(far).is_empty());
        assert_eq!(v.contact_at_column(far, HIGH_ABOVE, &(0..12)), None);
    }

    #[test]
    fn a_capsule_claims_its_own_length_and_nothing_past_its_flanks() {
        let v = voids(vec![capsule(0.0, ProceduralContact::Seal)]);
        assert_eq!(
            v.contact_at_column(Vec2::new(100, 0), HIGH_ABOVE, &(0..8)),
            Some(ProceduralContact::Seal),
            "the midpoint of the capsule"
        );
        assert_eq!(
            v.contact_at_column(Vec2::new(100, 500), HIGH_ABOVE, &(0..8)),
            None,
            "far off the capsule's flank"
        );
    }

    /// A `Connect` capsule, like a `Connect` disc, is tested against its exact
    /// carved radius.
    #[test]
    fn a_connect_capsule_has_no_margin_either() {
        let v = voids(vec![capsule(0.0, ProceduralContact::Connect)]);
        assert_eq!(
            v.contact_at_column(Vec2::new(100, 9), HIGH_ABOVE, &(0..8)),
            Some(ProceduralContact::Connect)
        );
        assert_eq!(
            v.contact_at_column(Vec2::new(100, 11), HIGH_ABOVE, &(0..8)),
            None
        );
    }

    /// The capsule must follow the *bowed* spline, not the straight chord
    /// between its endpoints -- the property a chord-based capsule would get
    /// wrong, and the one that makes this index agree with the real carve.
    #[test]
    fn a_bowed_capsule_follows_the_spline_and_not_the_chord() {
        let claimed = |curve: f32| -> Vec<i32> {
            let v = voids(vec![capsule(curve, ProceduralContact::Connect)]);
            (-400..=400)
                .filter(|y| {
                    v.contact_at_column(Vec2::new(100, *y), HIGH_ABOVE, &(0..8))
                        .is_some()
                })
                .collect()
        };
        let straight = claimed(0.0);
        let bowed = claimed(0.4);
        assert!(
            !straight.is_empty() && !bowed.is_empty(),
            "both shapes must claim something at their midpoint column"
        );
        assert!(
            straight.contains(&0),
            "the straight capsule's centreline IS the chord"
        );
        assert!(
            !bowed.contains(&0),
            "a bowed capsule must move its centreline off the chord entirely; if the chord is \
             still inside the shape, the capsule is not following the spline"
        );
    }

    /// The bucket grid is an optimisation, never a filter: whatever the exact
    /// test would claim, the bucket must contain the shape that claims it.
    #[test]
    fn the_bucket_grid_never_drops_a_shape_the_exact_test_would_claim() {
        let indexed = voids(vec![
            disc(ProceduralContact::Seal),
            capsule(0.15, ProceduralContact::Connect),
            TestShape::Capsule(
                CapsuleShape {
                    a: Vec3::new(-300, -120, 0),
                    b: Vec3::new(160, 340, 0),
                    r_a: 14.0,
                    r_b: 6.0,
                    curve: -0.25,
                    span: CapsuleSpan::AboveFloor { headroom: 10.0 },
                },
                ProceduralContact::Seal,
            ),
        ]);
        // The same shapes, but with every shape considered at every column:
        // the brute-force answer the grid must reproduce exactly.
        let brute: Vec<u32> = (0..indexed.shapes.len() as u32).collect();
        for y in (-400..=400).step_by(7) {
            for x in (-400..=400).step_by(7) {
                let wpos2d = Vec2::new(x, y);
                let z = 0..10;
                let via_grid = indexed.contact_at_column(wpos2d, HIGH_ABOVE, &z);
                let via_brute = ChunkVoids {
                    voids: &indexed,
                    shapes: &brute,
                }
                .contact_at_column(wpos2d, HIGH_ABOVE, &z);
                assert_eq!(
                    via_grid, via_brute,
                    "grid and brute-force disagree at {wpos2d:?}"
                );
            }
        }
    }

    #[test]
    fn intersects_column_is_policy_blind() {
        let seal = voids(vec![disc(ProceduralContact::Seal)]);
        let connect = voids(vec![disc(ProceduralContact::Connect)]);
        assert!(seal.intersects_column(Vec2::new(5, 0), HIGH_ABOVE, &(0..12)));
        assert!(connect.intersects_column(Vec2::new(5, 0), HIGH_ABOVE, &(0..12)));
    }

    /// The index must mirror the authored geometry exactly -- one shape per
    /// carved shape, no more and no fewer -- and each shape must carry the
    /// policy of the feature it came from, with every interior shape sealed.
    ///
    /// Needs the real authored assets pulled locally:
    /// `cargo test -p xindeler-world authored_voids -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn the_real_index_mirrors_the_authored_geometry_one_shape_at_a_time() {
        use crate::{CanvasInfo, layer::authored_regions::authored_voids};

        let threadpool = rayon::ThreadPoolBuilder::new().build().unwrap();
        let (world, index) = crate::World::generate(
            0,
            crate::sim::WorldOpts {
                seed_elements: true,
                world_file: crate::sim::FileOpts::LoadAsset("world.map.cromatolis_v0".to_string()),
                calendar: None,
            },
            &threadpool,
            &|_| {},
        );
        let index_ref = index.as_index_ref();
        CanvasInfo::with_mock_canvas_info(index_ref, world.sim(), |info| {
            let voids = authored_voids(info).expect("the authored region must index its voids");
            let caves = index_ref
                .cromatolis_cave_features
                .get()
                .expect("the void index must have populated the cave cache");
            let interiors = index_ref
                .cromatolis_interiors
                .get()
                .expect("the void index must have populated the interior cache");

            // One disc for the hub, one capsule per branch.
            let expected_cave_shapes: usize =
                caves.iter().map(|cave| 1 + cave.branch_count()).sum();
            let expected_interior_shapes: usize = interiors
                .iter()
                .map(|i| i.void_discs().count() + i.void_capsules().count())
                .sum();
            assert_eq!(
                voids.len(),
                expected_cave_shapes + expected_interior_shapes,
                "the index must hold exactly one shape per authored carved shape"
            );

            // Every shape of a cave carries that cave's policy; every shape of
            // an interior is sealed.
            let expected_connect: usize = caves
                .iter()
                .filter(|cave| cave.procedural_contact() == ProceduralContact::Connect)
                .map(|cave| 1 + cave.branch_count())
                .sum();
            let (seal, connect) = voids.policy_counts();
            assert_eq!(connect, expected_connect);
            assert_eq!(
                seal,
                expected_cave_shapes + expected_interior_shapes - expected_connect
            );

            let sealed_caves = caves
                .iter()
                .filter(|c| c.procedural_contact() == ProceduralContact::Seal)
                .count();
            println!(
                "authored caves {} ({sealed_caves} Seal / {} Connect), interiors {}\n  indexed \
                 shapes {} ({seal} Seal / {connect} Connect), of which {expected_interior_shapes} \
                 from interiors",
                caves.len(),
                caves.len() - sealed_caves,
                interiors.len(),
                voids.len(),
            );
        });
    }

    /// The `Seal` dilation applies past a capsule's *ends*, not only along its
    /// flanks. That holds because the spline solver clamps its parameter into
    /// `[0, 1]`, so a column beyond an endpoint measures its distance to that
    /// endpoint -- i.e. the shape has rounded caps. If a future change to the
    /// solver stopped clamping, a tunnel could meet an authored branch tip
    /// head-on and stop one block short of it, leaving exactly the paper-thin
    /// membrane the margin exists to prevent.
    #[test]
    fn a_seal_capsule_is_dilated_past_its_end_caps_too() {
        let v = voids(vec![capsule(0.0, ProceduralContact::Seal)]);
        // The capsule runs x = 0..200 with radius 10; margin 16 + slack 3.
        assert_eq!(
            v.contact_at_column(Vec2::new(225, 0), HIGH_ABOVE, &(0..8)),
            Some(ProceduralContact::Seal),
            "25 blocks past the end is inside radius 10 + margin 19"
        );
        assert_eq!(
            v.contact_at_column(Vec2::new(232, 0), HIGH_ABOVE, &(0..8)),
            None,
            "32 blocks past the end is outside it"
        );
    }

    /// A `Connect` capsule, which is never dilated, must stop exactly where the
    /// carve does -- including at its end caps.
    #[test]
    fn a_connect_capsule_stops_at_its_own_end_cap() {
        let v = voids(vec![capsule(0.0, ProceduralContact::Connect)]);
        assert_eq!(
            v.contact_at_column(Vec2::new(208, 0), HIGH_ABOVE, &(0..8)),
            Some(ProceduralContact::Connect),
            "8 blocks past the end is still inside the carved radius of 10"
        );
        assert_eq!(
            v.contact_at_column(Vec2::new(212, 0), HIGH_ABOVE, &(0..8)),
            None,
            "12 blocks past the end is outside it"
        );
    }

    /// A surface-anchored (liquid) capsule drops its *whole* band when the
    /// surface cap binds, the way the carve does -- not just its ceiling.
    /// Modelling it floor-anchored leaves the protected floor above the real
    /// carved bottom, which under-protects exactly where the terrain is
    /// shallowest.
    #[test]
    fn a_surface_anchored_capsule_drops_its_floor_with_the_cap() {
        let water = TestShape::Capsule(
            CapsuleShape {
                a: Vec3::new(0, 0, 100),
                b: Vec3::new(200, 0, 100),
                r_a: 10.0,
                r_b: 10.0,
                curve: 0.0,
                span: CapsuleSpan::BelowSurface { depth: 6.0 },
            },
            ProceduralContact::Connect,
        );
        let v = voids(vec![water]);
        // Uncapped: the band is [94, 100].
        assert_eq!(
            v.contact_at_column(Vec2::new(100, 0), HIGH_ABOVE, &(94..95)),
            Some(ProceduralContact::Connect)
        );
        assert_eq!(
            v.contact_at_column(Vec2::new(100, 0), HIGH_ABOVE, &(88..89)),
            None
        );
        // Terrain at 94 caps the surface to 90, so the carve produces [84, 90]
        // -- the floor moves down with it.
        assert_eq!(
            v.contact_at_column(Vec2::new(100, 0), 94.0, &(85..86)),
            Some(ProceduralContact::Connect),
            "the capped band's floor must follow the cap, not stay at 94"
        );
        assert_eq!(
            v.contact_at_column(Vec2::new(100, 0), 94.0, &(95..96)),
            None,
            "and its ceiling must be the cap, not the uncapped surface"
        );
    }

    /// The protection index is *defined* as the carved volume dilated by one
    /// margin, and that equality rests on three byte-identical copies of the
    /// spline sample agreeing -- this module's, and the two carve modules'.
    /// A one-sided edit to any of them would silently mis-protect, with only
    /// an `#[ignore]`d real-asset test to catch it. This makes the invariant
    /// load-bearing instead of aspirational.
    #[test]
    fn all_three_spline_samples_agree() {
        let cases = [
            (Vec2::new(0.0, 0.0), Vec2::new(200.0, 0.0), 0.0),
            (Vec2::new(0.0, 0.0), Vec2::new(200.0, 0.0), 0.3),
            (Vec2::new(-90.5, 40.5), Vec2::new(160.5, -220.5), -0.27),
            (Vec2::new(12.5, 12.5), Vec2::new(-300.5, 75.5), 0.18),
        ];
        let mut sampled = 0;
        for (a2, b2, curve) in cases {
            for gx in -20..=20 {
                for gy in -20..=20 {
                    let point = Vec2::new(gx as f64 * 17.5, gy as f64 * 17.5);
                    let ours = spline_sample(a2, b2, curve, point);
                    let cave = crate::layer::cromatolis_cave_features::spline_sample_for_parity(
                        a2, b2, curve, point,
                    );
                    let interior = crate::layer::cromatolis_interior::spline_sample_for_parity(
                        a2, b2, curve, point,
                    );
                    assert_eq!(ours, cave, "cave copy diverged at {point:?}");
                    assert_eq!(ours, interior, "interior copy diverged at {point:?}");
                    sampled += 1;
                }
            }
        }
        assert!(sampled > 1_000);
    }

    /// A world with no authored region indexes nothing, so both paths through
    /// the choke point must agree exactly, tunnel for tunnel. That is what
    /// makes a purely procedural world bit-identical to having no guard in the
    /// tree at all.
    #[test]
    #[ignore]
    fn a_purely_procedural_world_indexes_no_authored_voids() {
        use crate::{
            CanvasInfo,
            layer::{
                authored_regions::authored_voids,
                cave::{tunnel_bounds_at, tunnel_bounds_at_unguarded},
            },
        };

        let threadpool = rayon::ThreadPoolBuilder::new().build().unwrap();
        let (world, index) = crate::World::generate(
            0,
            crate::sim::WorldOpts {
                seed_elements: true,
                world_file: crate::sim::FileOpts::Generate(Default::default()),
                calendar: None,
            },
            &threadpool,
            &|_| {},
        );
        let index_ref = index.as_index_ref();
        CanvasInfo::with_mock_canvas_info(index_ref, world.sim(), |info| {
            assert!(
                authored_voids(info).is_none(),
                "a world with no authored region must index no authored voids, so every consumer \
                 of the index is a no-op there"
            );
            let land = info.land();
            let mut columns_with_tunnels = 0;
            for i in 0..2_000 {
                let wpos2d = Vec2::new((i % 50) * 137, (i / 50) * 149);
                let guarded: Vec<_> = tunnel_bounds_at(wpos2d, info, &land)
                    .map(|(level, z, ..)| (level, z))
                    .collect();
                let unguarded: Vec<_> = tunnel_bounds_at_unguarded(wpos2d, info, &land)
                    .map(|(level, z, ..)| (level, z))
                    .collect();
                assert_eq!(guarded, unguarded, "at {wpos2d:?}");
                columns_with_tunnels += usize::from(!guarded.is_empty());
            }
            assert!(
                columns_with_tunnels > 0,
                "the comparison is worthless if no sampled column had a tunnel"
            );
        });
    }

    #[test]
    fn policy_counts_track_what_was_registered() {
        let v = voids(vec![
            disc(ProceduralContact::Seal),
            disc(ProceduralContact::Connect),
            capsule(0.0, ProceduralContact::Connect),
        ]);
        assert_eq!(v.len(), 3);
        assert_eq!(v.policy_counts(), (1, 2));
    }
}
