use bitvec::prelude::{BitBox, bitbox};
use common::{
    terrain::{MapSizeLg, TerrainChunkSize, neighbors, uniform_idx_as_vec2, vec2_as_uniform_idx},
    vol::RectVolSize,
};
use common_base::prof_span;
use noise::{
    MultiFractal, NoiseFn, Seedable, core::worley::*, math::vectors::*,
    permutationtable::PermutationTable,
};
use num::Float;
use rayon::prelude::*;
use std::sync::Arc;
use vek::*;

/// Calculates the smallest distance along an axis (x, y) from an edge of
/// the world.  This value is maximal at map_size_lg.chunks() / 2 and
/// minimized at the
/// extremes (0 or map_size_lg.chunks() on one or more axes).  It then divides
/// the quantity by cell_size, so the final result is 1 when we are not in a
/// cell along the edge of the world, and ranges between 0 and 1 otherwise
/// (lower when the chunk is closer to the edge).
pub fn map_edge_factor(map_size_lg: MapSizeLg, posi: usize) -> f32 {
    uniform_idx_as_vec2(map_size_lg, posi)
        .map2(map_size_lg.chunks().map(i32::from), |e, sz| {
            (sz / 2 - (e - sz / 2).abs()) as f32 / (16.0 / 1024.0 * sz as f32)
        })
        .reduce_partial_min()
        .clamp(0.0, 1.0)
}

/// Computes the cumulative distribution function of the weighted sum of k
/// independent, uniformly distributed random variables between 0 and 1.  For
/// each variable i, we use `weights[i]` as the weight to give `samples[i]` (the
/// weights should all be positive).
///
/// If the precondition is met, the distribution of the result of calling this
/// function will be uniformly distributed while preserving the same information
/// that was in the original average.
///
/// For N > 33 the function will no longer return correct results since we will
/// overflow u32.
///
/// NOTE:
///
/// Per [[1]], the problem of determining the CDF of
/// the sum of uniformly distributed random variables over *different* ranges is
/// considerably more complicated than it is for the same-range case.
/// Fortunately, it also provides a reference to [2], which contains a complete
/// derivation of an exact rule for the density function for this case.  The CDF
/// is just the integral of the cumulative distribution function [[3]],
/// which we use to convert this into a CDF formula.
///
/// This allows us to sum weighted, uniform, independent random variables.
///
/// At some point, we should probably contribute this back to stats-rs.
///
/// 1. [https://www.r-bloggers.com/sums-of-random-variables/][1],
/// 2. Sadooghi-Alvandi, S., A. Nematollahi, & R. Habibi, 2009. On the
///    Distribution of the Sum of Independent Uniform Random Variables.
///    Statistical Papers, 50, 171-175.
/// 3. [https://en.wikipedia.org/wiki/Cumulative_distribution_function][3]
///
/// [1]: https://www.r-bloggers.com/sums-of-random-variables/
/// [3]: https://en.wikipedia.org/wiki/Cumulative_distribution_function
pub fn cdf_irwin_hall<const N: usize>(weights: &[f32; N], samples: [f32; N]) -> f32 {
    // Let J_k = {(j_1, ... , j_k) : 1 ≤ j_1 < j_2 < ··· < j_k ≤ N }.
    //
    // Let A_N = Π{k = 1 to n}a_k.
    //
    // The density function for N ≥ 2 is:
    //
    //   1/(A_N * (N - 1)!) * (x^(N-1) + Σ{k = 1 to N}((-1)^k *
    //   Σ{(j_1, ..., j_k) ∈ J_k}(max(0, x - Σ{l = 1 to k}(a_(j_l)))^(N - 1))))
    //
    // So the cumulative distribution function is its integral, i.e. (I think)
    //
    // 1/(product{k in A}(k) * N!) * (x^N + sum(k in 1 to N)((-1)^k *
    // sum{j in Subsets[A, {k}]}(max(0, x - sum{l in j}(l))^N)))
    //
    // which is also equivalent to
    //
    //   (letting B_k = { a in Subsets[A, {k}] : sum {l in a} l }, B_(0,1) = 0 and
    //            H_k = { i : 1 ≤ 1 ≤ N! / (k! * (N - k)!) })
    //
    //   1/(product{k in A}(k) * N!) * sum(k in 0 to N)((-1)^k *
    //   sum{l in H_k}(max(0, x - B_(k,l))^N))
    //
    // We should be able to iterate through the whole power set
    // instead, and figure out K by calling count_ones(), so we can compute the
    // result in O(2^N) iterations.
    let x: f64 = weights
        .iter()
        .zip(samples.iter())
        .map(|(&weight, &sample)| weight as f64 * sample as f64)
        .sum();

    let mut y = 0.0f64;
    for subset in 0u32..(1 << N) {
        // Number of set elements
        let k = subset.count_ones();
        // Add together exactly the set elements to get B_subset
        let z = weights
            .iter()
            .enumerate()
            .filter(|(i, _)| subset & (1 << i) as u32 != 0)
            .map(|(_, &k)| k as f64)
            .sum::<f64>();
        // Compute max(0, x - B_subset)^N
        let z = (x - z).max(0.0).powi(N as i32);
        // The parity of k determines whether the sum is negated.
        y += if k & 1 == 0 { z } else { -z };
    }

    // Divide by the product of the weights.
    y /= weights.iter().map(|&k| k as f64).product::<f64>();

    // Remember to multiply by 1 / N! at the end.
    (y / (1..(N as i32) + 1).product::<i32>() as f64) as f32
}

/// First component of each element of the vector is the computed CDF of the
/// noise function at this index (i.e. its position in a sorted list of value
/// returned by the noise function applied to every chunk in the game).  Second
/// component is the cached value of the noise function that generated the
/// index.
pub type InverseCdf<F = f32> = Box<[(f32, F)]>;

/// NOTE: First component is estimated horizon angles at each chunk; second
/// component is estimated       heights of maximal occluder at each chunk (used
/// for making shadows volumetric).
pub type HorizonMap<A, H> = (Vec<A>, Vec<H>);

/// Compute inverse cumulative distribution function for arbitrary function f,
/// the hard way.  We pre-generate noise values prior to worldgen, then sort
/// them in order to determine the correct position in the sorted order.  That
/// lets us use `(index + 1) / (WORLDSIZE.y * WORLDSIZE.x)` as a uniformly
/// distributed (from almost-0 to 1) regularization of the chunks.  That is, if
/// we apply the computed "function" F⁻¹(x, y) to (x, y) and get out p, it means
/// that approximately (100 * p)% of chunks have a lower value for F⁻¹ than p.
/// The main purpose of doing this is to make sure we are using the entire range
/// we want, and to allow us to apply the numerous results about distributions
/// on uniform functions to the procedural noise we generate, which lets us much
/// more reliably control the *number* of features in the world while still
/// letting us play with the *shape* of those features, without having arbitrary
/// cutoff points / discontinuities (which tend to produce ugly-looking /
/// unnatural terrain).
///
/// As a concrete example, before doing this it was very hard to tweak humidity
/// so that either most of the world wasn't dry, or most of it wasn't wet, by
/// combining the billow noise function and the computed altitude.  This is
/// because the billow noise function has a very unusual distribution that is
/// heavily skewed towards 0.  By correcting for this tendency, we can start
/// with uniformly distributed billow noise and altitudes and combine them to
/// get uniformly distributed humidity, while still preserving the existing
/// shapes that the billow noise and altitude functions produce.
///
/// f takes an index, which represents the index corresponding to this chunk in
/// any any SimChunk vector returned by uniform_noise, and (for convenience) the
/// float-translated version of those coordinates.
/// f should return a value with no NaNs.  If there is a NaN, it will panic.
/// There are no other conditions on f.  If f returns None, the value will be
/// set to NaN, and will be ignored for the purposes of computing the uniform
/// range.
///
/// Returns a vec of (f32, f32) pairs consisting of the percentage of chunks
/// with a value lower than this one, and the actual noise value (we don't need
/// to cache it, but it makes ensuring that subsequent code that needs the noise
/// value actually uses the same one we were using here easier).  Also returns
/// the "inverted index" pointing from a position to a noise.
pub fn uniform_noise<F: Float + Send>(
    map_size_lg: MapSizeLg,
    f: impl Fn(usize, Vec2<f64>) -> Option<F> + Sync,
) -> (InverseCdf<F>, Box<[(usize, F)]>) {
    let mut noise = (0..map_size_lg.chunks_len())
        .into_par_iter()
        .filter_map(|i| {
            f(
                i,
                (uniform_idx_as_vec2(map_size_lg, i)
                    * TerrainChunkSize::RECT_SIZE.map(|e| e as i32))
                .map(|e| e as f64),
            )
            .map(|res| (i, res))
        })
        .collect::<Vec<_>>();

    // sort_unstable_by is equivalent to sort_by here since we include a unique
    // index in the comparison.  We could leave out the index, but this might
    // make the order not reproduce the same way between different versions of
    // Rust (for example).
    noise.par_sort_unstable_by(|f, g| (f.1, f.0).partial_cmp(&(g.1, g.0)).unwrap());

    // Construct a vector that associates each chunk position with the 1-indexed
    // position of the noise in the sorted vector (divided by the vector length).
    // This guarantees a uniform distribution among the samples (excluding those
    // that returned None, which will remain at zero).
    let mut uniform_noise = vec![(0.0, F::nan()); map_size_lg.chunks_len()].into_boxed_slice();
    // NOTE: Consider using try_into here and elsewhere in this function, since
    // i32::MAX technically doesn't fit in an f32 (even if we should never reach
    // that limit).
    let total = noise.len() as f32;
    for (noise_idx, &(chunk_idx, noise_val)) in noise.iter().enumerate() {
        uniform_noise[chunk_idx] = ((1 + noise_idx) as f32 / total, noise_val);
    }
    (uniform_noise, noise.into_boxed_slice())
}

/// Iterate through all cells adjacent and including four chunks whose top-left
/// point is posi. This isn't just the immediate neighbors of a chunk plus the
/// center, because it is designed to cover neighbors of a point in the chunk's
/// "interior."
///
/// This is what's used during cubic interpolation, for example, as it
/// guarantees that for any point between the given chunk (on the top left) and
/// its top-right/down-right/down neighbors, the twelve chunks surrounding this
/// box (its "perimeter") are also inspected.
pub fn local_cells(map_size_lg: MapSizeLg, posi: usize) -> impl Clone + Iterator<Item = usize> {
    let pos = uniform_idx_as_vec2(map_size_lg, posi);
    // NOTE: want to keep this such that the chunk index is in ascending order!
    let grid_size = 3i32;
    let grid_bounds = 2 * grid_size + 1;
    (0..grid_bounds * grid_bounds)
        .map(move |index| {
            Vec2::new(
                pos.x + (index % grid_bounds) - grid_size,
                pos.y + (index / grid_bounds) - grid_size,
            )
        })
        .filter(move |pos| {
            pos.x >= 0
                && pos.y >= 0
                && pos.x < map_size_lg.chunks().x as i32
                && pos.y < map_size_lg.chunks().y as i32
        })
        .map(move |e| vec2_as_uniform_idx(map_size_lg, e))
}

// Note that we should already have okay cache locality since we have a grid.
pub fn uphill(
    map_size_lg: MapSizeLg,
    dh: &[isize],
    posi: usize,
) -> impl Clone + Iterator<Item = usize> + '_ {
    neighbors(map_size_lg, posi).filter(move |&posj| dh[posj] == posi as isize)
}

/// Compute the neighbor "most downhill" from all chunks.
///
/// TODO: See if allocating in advance is worthwhile.
pub fn downhill<F: Float>(
    map_size_lg: MapSizeLg,
    h: impl Fn(usize) -> F + Sync,
    is_ocean: impl Fn(usize) -> bool + Sync,
) -> Box<[isize]> {
    // Constructs not only the list of downhill nodes, but also computes an ordering
    // (visiting nodes in order from roots to leaves).
    (0..map_size_lg.chunks_len())
        .into_par_iter()
        .map(|posi| {
            let nh = h(posi);
            if is_ocean(posi) {
                -2
            } else {
                let mut best = -1;
                let mut besth = nh;
                for nposi in neighbors(map_size_lg, posi) {
                    let nbh = h(nposi);
                    if nbh < besth {
                        besth = nbh;
                        best = nposi as isize;
                    }
                }
                best
            }
        })
        .collect::<Vec<_>>()
        .into_boxed_slice()
}

/* /// Bilinear interpolation.
///
/// Linear interpolation in both directions (i.e. quadratic interpolation).
fn get_interpolated_bilinear<T, F>(&self, pos: Vec2<i32>, mut f: F) -> Option<T>
    where
        T: Copy + Default + Signed + Float + Add<Output = T> + Mul<f32, Output = T>,
        F: FnMut(Vec2<i32>) -> Option<T>,
{
    // (i) Find downhill for all four points.
    // (ii) Compute distance from each downhill point and do linear interpolation on
    // their heights. (iii) Compute distance between each neighboring point
    // and do linear interpolation on       their distance-interpolated
    // heights.

    // See http://articles.adsabs.harvard.edu/cgi-bin/nph-iarticle_query?1990A%26A...239..443S&defaultprint=YES&page_ind=0&filetype=.pdf
    //
    // Note that these are only guaranteed monotone in one dimension; fortunately,
    // that is sufficient for our purposes.
    let pos = pos.map2(TerrainChunkSize::RECT_SIZE, |e, sz: u32| {
        e as f64 / sz as f64
    });

    // Orient the chunk in the direction of the most downhill point of the four.  If
    // there is no "most downhill" point, then we don't care.
    let x0 = pos.map2(Vec2::new(0, 0), |e, q| e.max(0.0) as i32 + q);
    let y0 = f(x0)?;

    let x1 = pos.map2(Vec2::new(1, 0), |e, q| e.max(0.0) as i32 + q);
    let y1 = f(x1)?;

    let x2 = pos.map2(Vec2::new(0, 1), |e, q| e.max(0.0) as i32 + q);
    let y2 = f(x2)?;

    let x3 = pos.map2(Vec2::new(1, 1), |e, q| e.max(0.0) as i32 + q);
    let y3 = f(x3)?;

    let z0 = y0
        .mul(1.0 - pos.x.fract() as f32)
        .mul(1.0 - pos.y.fract() as f32);
    let z1 = y1.mul(pos.x.fract() as f32).mul(1.0 - pos.y.fract() as f32);
    let z2 = y2.mul(1.0 - pos.x.fract() as f32).mul(pos.y.fract() as f32);
    let z3 = y3.mul(pos.x.fract() as f32).mul(pos.y.fract() as f32);

    Some(z0 + z1 + z2 + z3)
} */

/// Find all ocean tiles from a height map, using an inductive definition of
/// ocean as one of:
/// - posi is at the side of the world (map_edge_factor(posi) == 0.0)
/// - posi has a neighboring ocean tile, and has a height below sea level
///   (oldh(posi) <= 0.0).
pub fn get_oceans<F: Float>(map_size_lg: MapSizeLg, oldh: impl Fn(usize) -> F + Sync) -> BitBox {
    // We can mark tiles as ocean candidates by scanning row by row, since the top
    // edge is ocean, the sides are connected to it, and any subsequent ocean
    // tiles must be connected to it.
    let mut is_ocean = bitbox![0; map_size_lg.chunks_len()];
    let mut stack = Vec::new();
    let mut do_push = |pos| {
        let posi = vec2_as_uniform_idx(map_size_lg, pos);
        if oldh(posi) <= F::zero() {
            stack.push(posi);
        }
    };
    for x in 0..map_size_lg.chunks().x as i32 {
        do_push(Vec2::new(x, 0));
        do_push(Vec2::new(x, map_size_lg.chunks().y as i32 - 1));
    }
    for y in 1..map_size_lg.chunks().y as i32 - 1 {
        do_push(Vec2::new(0, y));
        do_push(Vec2::new(map_size_lg.chunks().x as i32 - 1, y));
    }
    while let Some(chunk_idx) = stack.pop() {
        // println!("Ocean chunk {:?}: {:?}", uniform_idx_as_vec2(map_size_lg,
        // chunk_idx), oldh(chunk_idx));
        let mut is_ocean = is_ocean.get_mut(chunk_idx).unwrap();
        if *is_ocean {
            continue;
        }
        *is_ocean = true;
        stack.extend(neighbors(map_size_lg, chunk_idx).filter(|&neighbor_idx| {
            // println!("Ocean neighbor: {:?}: {:?}", uniform_idx_as_vec2(map_size_lg,
            // neighbor_idx), oldh(neighbor_idx));
            oldh(neighbor_idx) <= F::zero()
        }));
    }
    is_ocean
}

/// Squared distance written into cells that are (so far) known to be inside
/// the mask. Any value comfortably larger than the largest squared distance a
/// real map can produce (`2 * 2^15 ^ 2` even at the maximum supported map
/// size) works; a *finite* sentinel rather than `f64::INFINITY` keeps the
/// lower-envelope intersection below from evaluating `inf - inf`. It also has
/// to stay small enough that adding a real squared distance to it is not lost
/// to `f64` rounding, which rules out something like `1e300`.
const DISTANCE_TRANSFORM_UNREACHED: f64 = 1.0e10;

/// One axis of Felzenszwalb & Huttenlocher's exact distance transform: given
/// the sampled function `f`, write `min_q ((p - q)^2 + f[q])` into `d`.
///
/// `v` (`n` entries) and `z` (`n + 1` entries) are the algorithm's scratch
/// buffers -- the indices of the parabolas currently in the lower envelope and
/// the boundaries between them -- taken as parameters so the caller can reuse
/// one allocation across every row/column instead of allocating per line.
fn distance_transform_axis(f: &[f64], d: &mut [f64], v: &mut [usize], z: &mut [f64]) {
    let n = f.len();
    debug_assert!(n > 0);
    debug_assert_eq!(d.len(), n);
    debug_assert_eq!(v.len(), n);
    debug_assert_eq!(z.len(), n + 1);

    // Build the lower envelope of the parabolas rooted at each sample.
    let mut k = 0;
    v[0] = 0;
    z[0] = f64::NEG_INFINITY;
    z[1] = f64::INFINITY;
    for q in 1..n {
        let qf = q as f64;
        let mut s;
        loop {
            let vk = v[k] as f64;
            s = ((f[q] + qf * qf) - (f[v[k]] + vk * vk)) / (2.0 * qf - 2.0 * vk);
            // `z[0]` is -inf, so a finite `s` can never fall through at `k ==
            // 0`; the explicit guard is only there so a non-finite input can't
            // underflow the index.
            if s <= z[k] && k > 0 {
                k -= 1;
            } else {
                break;
            }
        }
        k += 1;
        v[k] = q;
        z[k] = s;
        z[k + 1] = f64::INFINITY;
    }

    // Walk the envelope once more to sample it.
    k = 0;
    for (q, d) in d.iter_mut().enumerate() {
        while z[k + 1] < q as f64 {
            k += 1;
        }
        let dq = q as f64 - v[k] as f64;
        *d = dq * dq + f[v[k]];
    }
}

/// Exact Euclidean distance transform of a binary mask: for every chunk
/// *inside* the mask, the distance (in chunks, not metres) to the nearest
/// chunk *outside* it; `0.0` for chunks that are themselves outside.
///
/// Returned *squared*, which on a lattice is always an exact integer and
/// therefore exactly comparable against another squared lattice distance.
/// Callers asking "is this chunk within distance `d` of that one" stay in this
/// space rather than taking two square roots and comparing the results, where
/// `sqrt(2) * sqrt(2)` lands on either side of `2` depending on the rounding.
///
/// NOTE: this is the distance to the nearest bank *from that chunk*, which is
/// only the half-width of the channel for a chunk sitting on the channel's
/// centre line. It is not a width signal -- see
/// [`local_channel_radius_chunks`], which is what "how wide is the channel
/// here" actually means.
///
/// Chunks outside the grid count as inside the mask (the transform only ever
/// finds banks that exist in the grid), so a region running off the map edge
/// over-reports its distance there. Immaterial for a mask that does not reach
/// the border; worth remembering for one that does.
///
/// This is Felzenszwalb & Huttenlocher's O(n) algorithm (one
/// [`distance_transform_axis`] pass per axis), so it is exact rather than a
/// chamfer approximation, and linear in the number of chunks. A mask with no
/// unset chunk at all saturates at `DISTANCE_TRANSFORM_UNREACHED` rather than
/// diverging.
fn squared_distance_to_unset_chunks(
    map_size_lg: MapSizeLg,
    is_set: impl Fn(usize) -> bool,
) -> Box<[f64]> {
    prof_span!("squared_distance_to_unset_chunks");
    let width = usize::from(map_size_lg.chunks().x);
    let height = usize::from(map_size_lg.chunks().y);

    let mut squared = (0..map_size_lg.chunks_len())
        .map(|posi| {
            if is_set(posi) {
                DISTANCE_TRANSFORM_UNREACHED
            } else {
                0.0
            }
        })
        .collect::<Vec<_>>();

    let axis_len = width.max(height);
    let mut line = vec![0.0; axis_len];
    let mut out = vec![0.0; axis_len];
    let mut v = vec![0usize; axis_len];
    let mut z = vec![0.0; axis_len + 1];

    // Rows first, then columns; the two passes compose into the exact 2D
    // transform (this is the separability property the algorithm relies on).
    for row in squared.chunks_exact_mut(width) {
        line[..width].copy_from_slice(row);
        distance_transform_axis(
            &line[..width],
            &mut out[..width],
            &mut v[..width],
            &mut z[..width + 1],
        );
        row.copy_from_slice(&out[..width]);
    }
    for x in 0..width {
        for y in 0..height {
            line[y] = squared[y * width + x];
        }
        distance_transform_axis(
            &line[..height],
            &mut out[..height],
            &mut v[..height],
            &mut z[..height + 1],
        );
        for y in 0..height {
            squared[y * width + x] = out[y];
        }
    }

    squared.into_boxed_slice()
}

/// Local channel radius: for every chunk inside the mask, the radius (in
/// chunks) of the *largest disc that fits inside the mask and covers this
/// chunk*. `0.0` outside the mask.
///
/// This -- not [`distance_to_unset_chunks`] -- is what "how wide is the
/// channel here" means, and the difference is not academic. A chunk's own
/// distance to the bank is small on *both* banks of a wide body, so
/// thresholding it directly selects the body's one-chunk-thick rim and leaves
/// the interior below the threshold: on the shipped Cromatolis corridor raster
/// that classified 11,650 rim chunks of 82 wide bodies as narrow channels
/// wrapped around 20,182 "wide" interior chunks. This function instead reports
/// the width of the channel the chunk *sits in*, so a whole body is uniformly
/// wide and only a genuinely narrow corridor comes out narrow -- while a river
/// that widens into a delta still transitions along its length, which a
/// per-connected-component maximum would not allow.
///
/// (Morphologically: `local_channel_radius(p) = max { d(q) : |p - q| <=
/// d(q) }`, the opening function of the mask.)
///
/// Cost is `O(sum of d(q)^2)` -- 1.9M chunk visits for the 33k-chunk Cromatolis
/// corridor raster, a few ms, once per worldgen. That bound is proportional to
/// the *area* of the widest region in the mask, so this is meant for corridor-
/// shaped masks, not for something like the full ocean.
pub fn local_channel_radius_chunks(
    map_size_lg: MapSizeLg,
    is_set: impl Fn(usize) -> bool,
) -> Box<[f32]> {
    prof_span!("local_channel_radius_chunks");
    // Worked in *squared* distances throughout, so "is this chunk inside that
    // disc" is an exact comparison of two integers rather than of two rounded
    // square roots.
    let squared_distance = squared_distance_to_unset_chunks(map_size_lg, is_set);
    let chunks = map_size_lg.chunks().map(i32::from);
    let mut squared_radius = vec![0.0f64; squared_distance.len()];

    // `max` is order-independent, so no sorting is needed: every chunk of the
    // mask is the centre of a fitting disc of its own radius, and each such
    // disc claims every chunk it covers.
    for (centre, &squared_r) in squared_distance.iter().enumerate() {
        if squared_r <= 0.0 {
            continue;
        }
        let centre_pos = uniform_idx_as_vec2(map_size_lg, centre);
        let reach = squared_r.sqrt() as i32;
        for dy in -reach..=reach {
            for dx in -reach..=reach {
                if f64::from(dx * dx + dy * dy) > squared_r {
                    continue;
                }
                let pos = centre_pos + Vec2::new(dx, dy);
                if pos.x < 0 || pos.y < 0 || pos.x >= chunks.x || pos.y >= chunks.y {
                    continue;
                }
                let covered = &mut squared_radius[vec2_as_uniform_idx(map_size_lg, pos)];
                if *covered < squared_r {
                    *covered = squared_r;
                }
            }
        }
    }

    squared_radius
        .into_iter()
        .map(|r| r.max(0.0).sqrt() as f32)
        .collect::<Vec<_>>()
        .into_boxed_slice()
}

/// Finds the horizon map for sunlight for the given chunks.
#[expect(clippy::result_unit_err)]
pub fn get_horizon_map<F: Float + Sync, A: Send, H: Send>(
    map_size_lg: MapSizeLg,
    bounds: Aabr<i32>,
    minh: F,
    maxh: F,
    h: impl Fn(usize) -> F + Sync,
    to_angle: impl Fn(F) -> A + Sync,
    to_height: impl Fn(F) -> H + Sync,
) -> Result<[HorizonMap<A, H>; 2], ()> {
    prof_span!("get_horizon_map");
    if maxh < minh {
        // maxh must be greater than minh
        return Err(());
    }
    let map_size = Vec2::<i32>::from(bounds.size()).map(|e| e as usize);
    let map_len = map_size.product();

    // Now, do the raymarching.
    let chunk_x = if let Vec2 { x: Some(x), .. } = TerrainChunkSize::RECT_SIZE.map(F::from) {
        x
    } else {
        return Err(());
    };
    // let epsilon = F::epsilon() * if let x = F::from(map_size.x) { x } else {
    // return Err(()) };
    let march = |dx: isize, maxdx: fn(isize, map_size_lg: MapSizeLg) -> isize| {
        let mut angles = Vec::with_capacity(map_len);
        let mut heights = Vec::with_capacity(map_len);
        (0..map_len)
            .into_par_iter()
            .map(|posi| {
                let wposi =
                    bounds.min + Vec2::new((posi % map_size.x) as i32, (posi / map_size.x) as i32);
                if wposi.reduce_partial_min() < 0
                    || wposi.x as usize >= usize::from(map_size_lg.chunks().x)
                    || wposi.y as usize >= usize::from(map_size_lg.chunks().y)
                {
                    return (to_angle(F::zero()), to_height(F::zero()));
                }
                let posi = vec2_as_uniform_idx(map_size_lg, wposi);
                // March in the given direction.
                let maxdx = maxdx(wposi.x as isize, map_size_lg);
                let mut slope = F::zero();
                let h0 = h(posi);
                let h = if h0 < minh {
                    F::zero()
                } else {
                    let mut max_height = F::zero();
                    let maxdz = maxh - h0;
                    let posi = posi as isize;
                    for deltax in 1..maxdx {
                        let posj = (posi + deltax * dx) as usize;
                        let deltax = chunk_x * F::from(deltax).unwrap();
                        let h_j_est = slope * deltax;
                        if h_j_est > maxdz {
                            break;
                        }
                        let h_j_act = h(posj) - h0;
                        if
                        /* h_j_est - h_j_act <= epsilon */
                        h_j_est <= h_j_act {
                            slope = h_j_act / deltax;
                            max_height = h_j_act;
                        }
                    }
                    h0 - minh + max_height
                };
                let a = slope;
                (to_angle(a), to_height(h))
            })
            .unzip_into_vecs(&mut angles, &mut heights);
        (angles, heights)
    };
    let west = march(-1, |x, _| x);
    let east = march(1, |x, map_size_lg| {
        (usize::from(map_size_lg.chunks().x) - x as usize) as isize
    });
    Ok([west, east])
}

fn build_sources<Source>(seed: u32, octaves: usize) -> Vec<Source>
where
    Source: Default + Seedable,
{
    let mut sources = Vec::with_capacity(octaves);
    for x in 0..octaves {
        let source = Source::default();
        sources.push(source.set_seed(seed + x as u32));
    }
    sources
}

/// Noise function that outputs hybrid Multifractal noise.
///
/// The result of this multifractal noise is that valleys in the noise should
/// have smooth bottoms at all altitudes.
///
/// Copied from noise crate to add offset.
#[derive(Clone, Debug)]
pub struct HybridMulti<T> {
    /// Total number of frequency octaves to generate the noise with.
    ///
    /// The number of octaves control the _amount of detail_ in the noise
    /// function. Adding more octaves increases the detail, with the drawback
    /// of increasing the calculation time.
    pub octaves: usize,

    /// The number of cycles per unit length that the noise function outputs.
    pub frequency: f64,

    /// A multiplier that determines how quickly the frequency increases for
    /// each successive octave in the noise function.
    ///
    /// The frequency of each successive octave is equal to the product of the
    /// previous octave's frequency and the lacunarity value.
    ///
    /// A lacunarity of 2.0 results in the frequency doubling every octave. For
    /// almost all cases, 2.0 is a good value to use.
    pub lacunarity: f64,

    /// A multiplier that determines how quickly the amplitudes diminish for
    /// each successive octave in the noise function.
    ///
    /// The amplitude of each successive octave is equal to the product of the
    /// previous octave's amplitude and the persistence value. Increasing the
    /// persistence produces "rougher" noise.
    ///
    /// H = 1.0 - fractal increment = -ln(persistence) / ln(lacunarity).  For
    /// a fractal increment between 0 (inclusive) and 1 (exclusive), keep
    /// persistence between 1 / lacunarity (inclusive, for low fractal
    /// dimension) and 1 (exclusive, for high fractal dimension).
    pub persistence: f64,

    /// An offset that is added to the output of each sample of the underlying
    /// Perlin noise function.  Because each successive octave is weighted in
    /// part by the previous signal's output, increasing the offset will weight
    /// the output more heavily towards 1.0.
    pub offset: f64,

    seed: u32,
    sources: Vec<T>,
    //scale_factor: f64,
}

impl<T> HybridMulti<T>
where
    T: Default + Seedable,
{
    pub const DEFAULT_FREQUENCY: f64 = 2.0;
    pub const DEFAULT_LACUNARITY: f64 = /* std::f64::consts::PI * 2.0 / 3.0 */ 2.0;
    pub const DEFAULT_OCTAVES: usize = 6;
    pub const DEFAULT_OFFSET: f64 = /* 0.25 *//* 0.5 */ 0.7;
    // -ln(2^(-0.25))/ln(2) = 0.25
    // 2^(-0.25) ~ 13/16
    pub const DEFAULT_PERSISTENCE: f64 = /* 0.25 *//* 0.5 */ 13.0 / 16.0;
    pub const DEFAULT_SEED: u32 = 0;
    pub const MAX_OCTAVES: usize = 32;

    pub fn new(seed: u32) -> Self {
        Self {
            seed,
            octaves: Self::DEFAULT_OCTAVES,
            frequency: Self::DEFAULT_FREQUENCY,
            lacunarity: Self::DEFAULT_LACUNARITY,
            persistence: Self::DEFAULT_PERSISTENCE,
            offset: Self::DEFAULT_OFFSET,
            sources: build_sources(seed, Self::DEFAULT_OCTAVES),
            //scale_factor: Self::calc_scale_factor(Self::DEFAULT_PERSISTENCE,
            // Self::DEFAULT_OCTAVES),
        }
    }

    pub fn set_offset(self, offset: f64) -> Self { Self { offset, ..self } }

    #[expect(dead_code)]
    pub fn set_sources(self, sources: Vec<T>) -> Self { Self { sources, ..self } }
    /*fn calc_scale_factor(persistence: f64, octaves: usize) -> f64 {
        let mut result = persistence;

        // Do octave 0
        let mut amplitude = persistence;
        let mut weight = result;
        let mut signal = amplitude;
        weight *= signal;

        result += signal;

        if octaves >= 1 {
            result += (1..=octaves).fold(0.0, |acc, _| {
                amplitude *= persistence;
                weight = weight.max(1.0);
                signal = amplitude;
                weight *= signal;
                acc + signal
            });
        }

        2.0 / result
    }*/
}

impl<T> Default for HybridMulti<T>
where
    T: Default + Seedable,
{
    fn default() -> Self { Self::new(Self::DEFAULT_SEED) }
}

impl<T> MultiFractal for HybridMulti<T>
where
    T: Default + Seedable,
{
    fn set_octaves(self, mut octaves: usize) -> Self {
        if self.octaves == octaves {
            return self;
        }

        octaves = octaves.clamp(1, Self::MAX_OCTAVES);
        Self {
            octaves,
            sources: build_sources(self.seed, octaves),
            //scale_factor: Self::calc_scale_factor(self.persistence, octaves),
            ..self
        }
    }

    fn set_frequency(self, frequency: f64) -> Self { Self { frequency, ..self } }

    fn set_lacunarity(self, lacunarity: f64) -> Self { Self { lacunarity, ..self } }

    fn set_persistence(self, persistence: f64) -> Self {
        Self {
            persistence,
            //scale_factor: Self::calc_scale_factor(persistence, self.octaves),
            ..self
        }
    }
}

impl<T> Seedable for HybridMulti<T>
where
    T: Default + Seedable,
{
    fn set_seed(self, seed: u32) -> Self {
        if self.seed == seed {
            return self;
        }

        Self {
            seed,
            sources: build_sources(seed, self.octaves),
            ..self
        }
    }

    fn seed(&self) -> u32 { self.seed }
}

/// 2-dimensional `HybridMulti` noise
impl<T> NoiseFn<f64, 2> for HybridMulti<T>
where
    T: NoiseFn<f64, 2>,
{
    fn get(&self, point: [f64; 2]) -> f64 {
        let mut point = Vector2::from(point);

        let mut attenuation = self.persistence;

        // First unscaled octave of function; later octaves are scaled.
        point *= self.frequency;
        // Offset and bias to scale into [offset - 1.0, 1.0 + offset] range.
        let bias = 1.0;
        let mut result =
            (self.sources[0].get(point.into_array()) + self.offset) * bias * self.persistence;
        let mut scale = self.persistence;
        let mut weight = result;

        // Spectral construction inner loop, where the fractal is built.
        for x in 1..self.octaves {
            // Prevent divergence.
            weight = weight.min(1.0);

            // Raise the spatial frequency.
            point *= self.lacunarity;

            // Get noise value, and scale it to the [offset - 1.0, 1.0 + offset] range.
            let mut signal = (self.sources[x].get(point.into_array()) + self.offset) * bias;

            // Scale the amplitude appropriately for this frequency.
            signal *= attenuation;

            scale += attenuation;

            // Increase the attenuation for the next octave, to be equal to persistence ^ (x
            // + 1)
            attenuation *= self.persistence;

            // Add it in, weighted by previous octave's noise value.
            result += weight * signal;

            // Update the weighting value.
            weight *= signal;
        }

        // Scale the result to the [-1,1] range
        //result * self.scale_factor
        (result / scale) / bias - self.offset
    }
}

/// 3-dimensional `HybridMulti` noise
impl<T> NoiseFn<f64, 3> for HybridMulti<T>
where
    T: NoiseFn<f64, 3>,
{
    fn get(&self, point: [f64; 3]) -> f64 {
        let mut point = Vector3::from(point);

        let mut attenuation = self.persistence;

        // First unscaled octave of function; later octaves are scaled.
        point *= self.frequency;
        // Offset and bias to scale into [offset - 1.0, 1.0 + offset] range.
        let bias = 1.0;
        let mut result =
            (self.sources[0].get(point.into_array()) + self.offset) * bias * self.persistence;
        let mut scale = self.persistence;
        let mut weight = result;

        // Spectral construction inner loop, where the fractal is built.
        for x in 1..self.octaves {
            // Prevent divergence.
            weight = weight.min(1.0);

            // Raise the spatial frequency.
            point *= self.lacunarity;

            // Get noise value, and scale it to the [0, 1.0] range.
            let mut signal = (self.sources[x].get(point.into_array()) + self.offset) * bias;

            // Scale the amplitude appropriately for this frequency.
            signal *= attenuation;

            scale += attenuation;

            // Increase the attenuation for the next octave, to be equal to persistence ^ (x
            // + 1)
            attenuation *= self.persistence;

            // Add it in, weighted by previous octave's noise value.
            result += weight * signal;

            // Update the weighting value.
            weight *= signal;
        }

        // Scale the result to the [-1,1] range
        //result * self.scale_factor
        (result / scale) / bias - self.offset
    }
}

/// 4-dimensional `HybridMulti` noise
impl<T> NoiseFn<f64, 4> for HybridMulti<T>
where
    T: NoiseFn<f64, 4>,
{
    fn get(&self, point: [f64; 4]) -> f64 {
        let mut point = Vector4::from(point);

        let mut attenuation = self.persistence;

        // First unscaled octave of function; later octaves are scaled.
        point *= self.frequency;
        // Offset and bias to scale into [offset - 1.0, 1.0 + offset] range.
        let bias = 1.0;
        let mut result =
            (self.sources[0].get(point.into_array()) + self.offset) * bias * self.persistence;
        let mut scale = self.persistence;
        let mut weight = result;

        // Spectral construction inner loop, where the fractal is built.
        for x in 1..self.octaves {
            // Prevent divergence.
            weight = weight.min(1.0);

            // Raise the spatial frequency.
            point *= self.lacunarity;

            // Get noise value, and scale it to the [0, 1.0] range.
            let mut signal = (self.sources[x].get(point.into_array()) + self.offset) * bias;

            // Scale the amplitude appropriately for this frequency.
            signal *= attenuation;

            scale += attenuation;

            // Increase the attenuation for the next octave, to be equal to persistence ^ (x
            // + 1)
            attenuation *= self.persistence;

            // Add it in, weighted by previous octave's noise value.
            result += weight * signal;

            // Update the weighting value.
            weight *= signal;
        }

        // Scale the result to the [-1,1] range
        //result * self.scale_factor
        (result / scale) / bias - self.offset
    }
}

/* code used by sharp in future – note: NoiseFn impl probably broken by noise crate upgrade from 0.7 to 0.9
/// Noise function that applies a scaling factor and a bias to the output value
/// from the source function.
///
/// The function retrieves the output value from the source function, multiplies
/// it with the scaling factor, adds the bias to it, then outputs the value.
pub struct ScaleBias<'a, F: 'a> {
    /// Outputs a value.
    pub source: &'a F,

    /// Scaling factor to apply to the output value from the source function.
    /// The default value is 1.0.
    pub scale: f64,

    /// Bias to apply to the scaled output value from the source function.
    /// The default value is 0.0.
    pub bias: f64,
}

impl<'a, F> ScaleBias<'a, F> {
    pub fn new(source: &'a F) -> Self {
        ScaleBias {
            source,
            scale: 1.0,
            bias: 0.0,
        }
    }

    pub fn set_scale(self, scale: f64) -> Self { ScaleBias { scale, ..self } }

    pub fn set_bias(self, bias: f64) -> Self { ScaleBias { bias, ..self } }
}

impl<'a, F: NoiseFn<T> + 'a, T> NoiseFn<T> for ScaleBias<'a, F> {
    #[cfg(not(target_os = "emscripten"))]
    fn get(&self, point: T) -> f64 { (self.source.get(point)).mul_add(self.scale, self.bias) }

    #[cfg(target_os = "emscripten")]
    fn get(&self, point: T) -> f64 { (self.source.get(point) * self.scale) + self.bias }
}
 */

/// Noise function that outputs Worley noise.
///
/// Copied from noise crate to make thread-safe.
#[derive(Clone)]
pub struct Worley {
    /// Specifies the distance function to use when calculating the boundaries
    /// of the cell.
    pub distance_function: Arc<DistanceFunction>,

    /// Signifies whether the distance from the borders of the cell should be
    /// returned, or the value for the cell.
    pub return_type: ReturnType,

    /// Frequency of the seed points.
    pub frequency: f64,

    seed: u32,
    perm_table: PermutationTable,
}

type DistanceFunction = dyn Fn(&[f64], &[f64]) -> f64 + Sync + Send;

impl Worley {
    //pub const DEFAULT_SEED: u32 = 0;
    pub const DEFAULT_FREQUENCY: f64 = 1.0;

    pub fn new(seed: u32) -> Self {
        Self {
            perm_table: PermutationTable::new(seed),
            seed,
            distance_function: Arc::new(distance_functions::euclidean),
            return_type: ReturnType::Value,
            frequency: Self::DEFAULT_FREQUENCY,
        }
    }

    /// Sets the distance function used by the Worley cells.
    pub fn set_distance_function<F>(self, function: F) -> Self
    where
        F: Fn(&[f64], &[f64]) -> f64 + 'static + Sync + Send,
    {
        Self {
            distance_function: Arc::new(function),
            ..self
        }
    }

    /// Enables or disables applying the distance from the nearest seed point
    /// to the output value.
    #[expect(dead_code)]
    pub fn set_return_type(self, return_type: ReturnType) -> Self {
        Self {
            return_type,
            ..self
        }
    }

    /// Sets the frequency of the seed points.
    pub fn set_frequency(self, frequency: f64) -> Self { Self { frequency, ..self } }
}

impl Default for Worley {
    fn default() -> Self { Self::new(0) }
}

impl Seedable for Worley {
    /// Sets the seed value used by the Worley cells.
    fn set_seed(self, seed: u32) -> Self {
        // If the new seed is the same as the current seed, just return self.
        if self.seed == seed {
            return self;
        }

        // Otherwise, regenerate the permutation table based on the new seed.
        Self {
            perm_table: PermutationTable::new(seed),
            seed,
            ..self
        }
    }

    fn seed(&self) -> u32 { self.seed }
}

impl NoiseFn<f64, 2> for Worley {
    fn get(&self, point: [f64; 2]) -> f64 {
        worley_2d(
            &self.perm_table,
            &*self.distance_function,
            self.return_type,
            Vector2::from(point) * self.frequency,
        )
    }
}

impl NoiseFn<f64, 3> for Worley {
    fn get(&self, point: [f64; 3]) -> f64 {
        worley_3d(
            &self.perm_table,
            &*self.distance_function,
            self.return_type,
            Vector3::from(point) * self.frequency,
        )
    }
}

impl NoiseFn<f64, 4> for Worley {
    fn get(&self, point: [f64; 4]) -> f64 {
        worley_4d(
            &self.perm_table,
            &*self.distance_function,
            self.return_type,
            Vector4::from(point) * self.frequency,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 8 x 8 chunks -- small enough to spell out by hand, big enough to fit a
    /// multi-chunk-wide corridor with clearance on both sides.
    fn tiny_map() -> MapSizeLg { MapSizeLg::new(Vec2 { x: 3, y: 3 }).expect("valid map size") }

    fn distances(set: &[(i32, i32)]) -> Vec<f32> {
        let map_size_lg = tiny_map();
        let set = set
            .iter()
            .map(|&(x, y)| vec2_as_uniform_idx(map_size_lg, Vec2::new(x, y)))
            .collect::<Vec<_>>();
        squared_distance_to_unset_chunks(map_size_lg, |posi| set.contains(&posi))
            .iter()
            .map(|d| d.sqrt() as f32)
            .collect()
    }

    fn at(distances: &[f32], x: i32, y: i32) -> f32 {
        distances[vec2_as_uniform_idx(tiny_map(), Vec2::new(x, y))]
    }

    #[test]
    fn distance_transform_is_zero_outside_the_mask() {
        let d = distances(&[(4, 4)]);
        assert_eq!(at(&d, 0, 0), 0.0);
        assert_eq!(at(&d, 4, 5), 0.0);
    }

    #[test]
    fn an_isolated_chunk_is_one_chunk_from_the_outside() {
        let d = distances(&[(4, 4)]);
        assert_eq!(at(&d, 4, 4), 1.0);
    }

    #[test]
    fn a_one_chunk_wide_corridor_stays_one_chunk_from_the_outside() {
        let corridor = (0..8).map(|y| (4, y)).collect::<Vec<_>>();
        let d = distances(&corridor);
        for y in 0..8 {
            assert_eq!(at(&d, 4, y), 1.0, "corridor chunk (4, {y})");
        }
    }

    #[test]
    fn a_three_chunk_wide_corridor_is_two_chunks_deep_at_its_centre() {
        let corridor = (0..8)
            .flat_map(|y| [(3, y), (4, y), (5, y)])
            .collect::<Vec<_>>();
        let d = distances(&corridor);
        assert_eq!(at(&d, 4, 3), 2.0, "centre of the corridor");
        assert_eq!(at(&d, 3, 3), 1.0, "corridor edge");
        assert_eq!(at(&d, 5, 3), 1.0, "corridor edge");
    }

    /// Diagonals are exactly what a cheap chamfer approximation gets wrong, so
    /// pin one: a plus-shaped blob's centre is `sqrt(2)` from the nearest unset
    /// chunk (diagonally out through a corner), not `2`.
    #[test]
    fn distance_transform_is_exactly_euclidean_on_diagonals() {
        let d = distances(&[(4, 4), (3, 4), (5, 4), (4, 3), (4, 5)]);
        assert!(
            (at(&d, 4, 4) - 2.0f32.sqrt()).abs() < 1e-5,
            "expected sqrt(2), got {}",
            at(&d, 4, 4)
        );
    }

    #[test]
    fn an_empty_mask_is_all_zero() {
        assert!(distances(&[]).iter().all(|d| *d == 0.0));
    }

    #[test]
    fn a_fully_set_mask_saturates_instead_of_diverging() {
        let d = squared_distance_to_unset_chunks(tiny_map(), |_| true);
        assert!(
            d.iter().all(|d| *d == DISTANCE_TRANSFORM_UNREACHED),
            "{:?}",
            &d[..4]
        );
    }

    fn radii(set: &[(i32, i32)]) -> Vec<f32> {
        let map_size_lg = tiny_map();
        let set = set
            .iter()
            .map(|&(x, y)| vec2_as_uniform_idx(map_size_lg, Vec2::new(x, y)))
            .collect::<Vec<_>>();
        local_channel_radius_chunks(map_size_lg, |posi| set.contains(&posi)).into_vec()
    }

    #[test]
    fn a_narrow_corridor_has_radius_one_everywhere() {
        let corridor = (0..8).map(|y| (4, y)).collect::<Vec<_>>();
        let r = radii(&corridor);
        for y in 0..8 {
            assert_eq!(at(&r, 4, y), 1.0, "corridor chunk (4, {y})");
        }
    }

    /// The regression this function exists for: a wide body's *rim* chunks are
    /// one chunk from the bank, but they sit inside a wide channel, so their
    /// channel radius is the body's, not 1. Thresholding
    /// `distance_to_unset_chunks` directly would call the rim narrow and the
    /// interior wide -- a carved ring around flat water.
    #[test]
    fn a_wide_body_reports_its_own_width_on_its_rim_too() {
        let body = (2..7)
            .flat_map(|y| (2..7).map(move |x| (x, y)))
            .collect::<Vec<_>>();
        let r = radii(&body);
        assert_eq!(at(&r, 4, 4), 3.0, "centre of the body");
        for (x, y) in [(2, 4), (6, 4), (4, 2), (4, 6)] {
            assert_eq!(
                at(&r, x, y),
                3.0,
                "rim chunk ({x}, {y}) must report the body's width, not its own distance to the \
                 bank"
            );
            assert_eq!(
                distances(&body)[vec2_as_uniform_idx(tiny_map(), Vec2::new(x, y))],
                1.0,
                "...even though its own distance to the bank is 1"
            );
        }
    }

    /// A corridor that widens along its length still transitions, which is why
    /// this is an opening rather than a per-connected-component maximum.
    #[test]
    fn a_corridor_that_widens_still_transitions_along_its_length() {
        let mut mask = (0..4).map(|y| (4, y)).collect::<Vec<_>>();
        mask.extend((4..8).flat_map(|y| (2..7).map(move |x| (x, y))));
        let r = radii(&mask);
        assert_eq!(at(&r, 4, 0), 1.0, "the narrow head stays narrow");
        assert!(
            at(&r, 4, 6) > 1.0,
            "the wide tail is wide: {}",
            at(&r, 4, 6)
        );
    }

    #[test]
    fn local_channel_radius_is_zero_outside_the_mask() {
        assert_eq!(at(&radii(&[(4, 4)]), 0, 0), 0.0);
    }
}
