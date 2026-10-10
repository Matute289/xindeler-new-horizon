//! Engine side of the probe: load the real Cromatolis world exactly as the
//! server does, then sample it. Everything that needs `xindeler-world` lives
//! here; `format.rs` stays engine-free.

use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
};

use common::{
    terrain::{Block, BlockKind, SpriteKind, TerrainChunk},
    vol::ReadVol,
};
use rayon::prelude::*;
use sha2::{Digest, Sha256};
use strum::IntoEnumIterator;
use vek::Vec2;
use world::{
    IndexOwned, World,
    sim::{self, WorldSim},
    util::Sampler,
};

use crate::format::{self, Dump, Header, SiteRec, class, flag};

pub type Res<T> = Result<T, Box<dyn std::error::Error + Send + Sync>>;

/// Asset specifier of the Cromatolis map (same one the server loads).
pub const MAP_ASSET: &str = "world.map.cromatolis_v0";
pub const CHUNK: i32 = 32;

pub struct Probe {
    pub world: World,
    pub index: IndexOwned,
    pub pool: rayon::ThreadPool,
    pub seed: u32,
    pub assets_root: PathBuf,
}

/// Generate the world the way `Server::new` does (`World::generate` with the
/// Cromatolis map asset), using the asset root already selected through
/// `VELOREN_ASSETS`.
pub fn load(seed: u32, threads: Option<usize>) -> Res<Probe> {
    let mut b = rayon::ThreadPoolBuilder::new();
    if let Some(n) = threads {
        b = b.num_threads(n);
    }
    let pool = b.build()?;
    let (world, index) = World::generate(
        seed,
        sim::WorldOpts {
            seed_elements: true,
            world_file: sim::FileOpts::LoadAsset(MAP_ASSET.to_string()),
            calendar: None,
        },
        &pool,
        &|_| {},
    );
    Ok(Probe {
        world,
        index,
        pool,
        seed,
        assets_root: common::assets::ASSETS_PATH.clone(),
    })
}

/// World size in blocks.
pub fn world_size(sim: &WorldSim) -> Vec2<i32> { sim.get_size().map(|e| e as i32 * CHUNK) }

/// Block class used by the dump (see `format::class`).
///
/// The engine places sprites (flowers, tufts, boulders, chests...) with a
/// per-chunk dynamic RNG (see `dynamic_rng` in `World::generate_chunk`), so
/// which cells hold a sprite, and whether it is solid, changes from run to
/// run. Terrain, water and ordinary structure blocks do not. Unless
/// `keep_sprites` is set, every sprite block is therefore recorded as air,
/// which keeps dumps byte-reproducible; with it, sprites are `SPRITE`.
pub fn classify(b: &Block, keep_sprites: bool) -> u8 {
    // Lava is `BlockKind::Lava` (0x12): it is neither a fluid kind nor solid
    // in the engine, so it has to be matched explicitly or it reads as air.
    if b.is_liquid() || is_lava(b) {
        class::LIQUID
    } else if matches!(b.get_sprite(), Some(s) if s != SpriteKind::Empty) {
        if keep_sprites {
            class::SPRITE
        } else {
            class::AIR
        }
    } else if b.is_solid() {
        if is_natural_kind(b.kind()) {
            class::GROUND
        } else {
            class::STRUCTURE
        }
    } else {
        class::AIR
    }
}

/// Whether the block is lava (class [`class::LIQUID`], marked per column by
/// [`flag::LAVA`] so verifiers can tell it from water).
pub fn is_lava(b: &Block) -> bool { b.kind() == BlockKind::Lava }

/// Whether `kind` is one of the natural terrain kinds (class 1 candidates).
fn is_natural_kind(k: BlockKind) -> bool {
    matches!(
        k,
        BlockKind::Rock
            | BlockKind::WeakRock
            | BlockKind::GlowingRock
            | BlockKind::GlowingWeakRock
            | BlockKind::Grass
            | BlockKind::Snow
            | BlockKind::Earth
            | BlockKind::Sand
            | BlockKind::Ice
    )
}

/// The highest z at which the column sampler itself places terrain: the
/// engine fills `z <= alt as i32` (see `world/src/block.rs`). `None` when the
/// sampler returned nothing (NaN altitude).
pub fn surface_cap(alt: f32) -> Option<i32> { alt.is_finite().then_some(alt as i32) }

/// [`classify`] plus the natural-vs-structure decision for natural block
/// kinds.
///
/// A `BlockKind` alone cannot tell terrain from a structure: `Block` carries
/// only a kind and a colour, and site/structure code places plain Rock, Earth
/// and Sand blocks. What does separate them is the column sampler: every block
/// of terrain it generates lies at or below `alt as i32` (the one exception is
/// Ice, frozen water at `water_level`, which sits above `alt` by design). A
/// natural-kind block above that `cap` was therefore placed by something
/// else (stone walls, keeps, wells, bridge decks, boulders, debris, authored
/// floating islands) and is recorded as [`class::STRUCTURE`]. Without a `cap`
/// the kind alone decides (client dumps use the in-process sampler, so they
/// always have one).
pub fn classify_at(b: &Block, z: i32, cap: Option<i32>, keep_sprites: bool) -> u8 {
    let c = classify(b, keep_sprites);
    match cap {
        Some(cap) if c == class::GROUND && z > cap && b.kind() != BlockKind::Ice => {
            class::STRUCTURE
        },
        _ => c,
    }
}

pub fn block_kind_codes() -> BTreeMap<String, u8> {
    BlockKind::iter()
        .map(|k| (k.to_string(), k as u8))
        .collect()
}

/// sha256 of every `cromatolis_v0*` file under `<assets>/world/map`.
pub fn asset_hashes(root: &Path) -> BTreeMap<String, String> {
    let mut out = BTreeMap::new();
    let Ok(rd) = std::fs::read_dir(root.join("world/map")) else {
        return out;
    };
    for e in rd.flatten() {
        let name = e.file_name().to_string_lossy().into_owned();
        if name.starts_with("cromatolis_v0")
            && let Ok(bytes) = std::fs::read(e.path())
        {
            let hex: String = Sha256::digest(&bytes)
                .iter()
                .map(|b| format!("{b:02x}"))
                .collect();
            out.insert(name, hex);
        }
    }
    out
}

/// Half-open box in world metres.
#[derive(Clone, Copy, Debug)]
pub struct Box2 {
    pub x0: i32,
    pub y0: i32,
    pub x1: i32,
    pub y1: i32,
}

impl Box2 {
    pub fn parse(s: &str) -> Res<Self> {
        let v: Vec<i32> = s
            .split(',')
            .map(|p| p.trim().parse::<i32>())
            .collect::<Result<_, _>>()
            .map_err(|e| format!("bad box {s:?}: {e}"))?;
        let [x0, y0, x1, y1] = v[..] else {
            return Err(format!("box needs 4 values x0,y0,x1,y1, got {s:?}").into());
        };
        if x1 <= x0 || y1 <= y0 {
            return Err(format!("empty box {s:?} (need x1>x0 and y1>y0)").into());
        }
        if x1.checked_sub(x0).is_none() || y1.checked_sub(y0).is_none() {
            return Err(format!("box {s:?} is too large").into());
        }
        Ok(Self { x0, y0, x1, y1 })
    }

    pub fn nx(&self) -> usize { (self.x1 - self.x0) as usize }

    pub fn ny(&self) -> usize { (self.y1 - self.y0) as usize }

    pub fn chunk_range(&self) -> (Vec2<i32>, Vec2<i32>) {
        (
            Vec2::new(self.x0.div_euclid(CHUNK), self.y0.div_euclid(CHUNK)),
            Vec2::new(
                (self.x1 - 1).div_euclid(CHUNK),
                (self.y1 - 1).div_euclid(CHUNK),
            ),
        )
    }
}

/// Default sampled z range for a box, from the sim table only: just under
/// the lowest chunk basement/altitude to 96 m above the highest
/// altitude/water level, over the chunks the box touches **plus a one-chunk
/// halo**: a column's altitude is a spline of the chunk knots `c-1..c+2`, so
/// it is pulled towards the neighbouring chunks (next to a 500 m step, the
/// chunks the box touches alone clipped 938 of 1024 columns, BUG-P8). Use
/// [`auto_z_range_for`] to also fold in the sampler's actual column values.
pub fn auto_z_range(sim: &WorldSim, b: Box2) -> (i32, i32) {
    let (c0, c1) = b.chunk_range();
    let (mut lo, mut hi) = (f32::MAX, f32::MIN);
    for cy in c0.y - 1..=c1.y + 1 {
        for cx in c0.x - 1..=c1.x + 1 {
            if let Some(c) = sim.get(Vec2::new(cx, cy)) {
                lo = lo.min(c.alt.min(c.basement));
                hi = hi.max(c.alt.max(c.water_alt));
            }
        }
    }
    (
        (lo.floor() as i32).saturating_sub(16).max(-4096),
        (hi.ceil() as i32).saturating_add(96),
    )
}

/// Column-sampler stride (m) of [`auto_z_range_for`].
const AUTO_Z_STRIDE: i32 = 4;

/// The automatic z range of a box: [`auto_z_range`] widened to the actual
/// `alt` / `water_level` the column sampler returns on a 4 m grid over the
/// box (its four corners are always included), so a surface the chunk knots
/// do not predict (spline overshoot, rivers, cliffs) is still inside the
/// range. The same 16 m below / 96 m above margins apply.
pub fn auto_z_range_for(p: &Probe, b: Box2) -> (i32, i32) {
    let (mut zmin, mut zmax) = auto_z_range(p.world.sim(), b);
    let ir = p.index.as_index_ref();
    let cg = p.world.sample_columns();
    let ys: Vec<i32> = (b.y0..b.y1)
        .step_by(AUTO_Z_STRIDE as usize)
        .chain(std::iter::once(b.y1 - 1))
        .collect();
    let (lo, hi) = p.pool.install(|| {
        ys.par_iter()
            .map(|&y| {
                let (mut lo, mut hi) = (f32::MAX, f32::MIN);
                for x in (b.x0..b.x1)
                    .step_by(AUTO_Z_STRIDE as usize)
                    .chain(std::iter::once(b.x1 - 1))
                {
                    if let Some(s) = cg.get((Vec2::new(x, y), ir, None)) {
                        if s.alt.is_finite() {
                            lo = lo.min(s.alt);
                            hi = hi.max(s.alt);
                        }
                        if s.water_level.is_finite() {
                            hi = hi.max(s.water_level);
                        }
                    }
                }
                (lo, hi)
            })
            .reduce(|| (f32::MAX, f32::MIN), |a, b| (a.0.min(b.0), a.1.max(b.1)))
    });
    if lo <= hi {
        zmin = zmin.min((lo.floor() as i32).saturating_sub(16).max(-4096));
        zmax = zmax.max((hi.ceil() as i32).saturating_add(96));
    }
    (zmin, zmax)
}

/// Bottom-to-top run builder. Run lengths never exceed the column height
/// (at most `MAX_HEIGHT` = 32767, checked when the z range is resolved), so
/// they always fit a `u16` and merging two runs cannot overflow.
struct Runs(Vec<(u8, u16)>);

impl Runs {
    fn push(&mut self, c: u8, n: i32) {
        debug_assert!((1..=format::MAX_HEIGHT).contains(&n), "run length {n}");
        let n = n as u16;
        match self.0.last_mut() {
            Some((lc, ln)) if *lc == c => *ln += n,
            _ => self.0.push((c, n)),
        }
    }
}

/// Classes of the column at chunk-local `rel` for `z in [zmin, zmax)`, as runs.
/// Blocks below/above the chunk's stored range are constant, so they are
/// emitted as one run each (two where the surface `cap` falls inside) instead
/// of being queried one by one. `cap` is [`surface_cap`] of the column.
#[cfg(test)]
pub(crate) fn column_runs(
    ch: &TerrainChunk,
    rel: Vec2<i32>,
    zmin: i32,
    zmax: i32,
    keep_sprites: bool,
    cap: Option<i32>,
) -> Vec<(u8, u16)> {
    column_runs_ex(ch, rel, zmin, zmax, keep_sprites, cap).0
}

/// [`column_runs`] plus the column flags only the probe can derive:
/// [`flag::LAVA`] (a lava block was seen) and [`flag::RIM_NO_SAMPLE`] (`cap`
/// is `None`: the column sampler returned nothing, see [`surface_cap`]).
/// OR them into the column's [`format::summarize`] flags.
pub(crate) fn column_runs_ex(
    ch: &TerrainChunk,
    rel: Vec2<i32>,
    zmin: i32,
    zmax: i32,
    keep_sprites: bool,
    cap: Option<i32>,
) -> (Vec<(u8, u16)>, u8) {
    let lava = std::cell::Cell::new(false);
    let class_at = |z: i32| {
        ch.get(rel.with_z(z)).map_or(class::UNLOADED, |b| {
            if is_lava(b) {
                lava.set(true);
            }
            classify_at(b, z, cap, keep_sprites)
        })
    };
    // A constant stretch `[a, b)`: classify its first block, and the part
    // above the cap separately when the cap splits it.
    let constant = |r: &mut Runs, a: i32, b: i32| {
        if b <= a {
            return;
        }
        match cap {
            Some(c) if a <= c && c < b - 1 => {
                r.push(class_at(a), c + 1 - a);
                r.push(class_at(c + 1), b - c - 1);
            },
            _ => r.push(class_at(a), b - a),
        }
    };
    let (cmin, cmax) = (ch.get_min_z(), ch.get_max_z());
    let mut r = Runs(Vec::with_capacity(8));
    let lo_end = cmin.clamp(zmin, zmax);
    constant(&mut r, zmin, lo_end);
    let hi_start = cmax.clamp(zmin, zmax).max(lo_end);
    for z in lo_end..hi_start {
        r.push(class_at(z), 1);
    }
    constant(&mut r, hi_start, zmax);
    let mut extra = 0;
    if lava.get() {
        extra |= flag::LAVA;
    }
    if cap.is_none() {
        extra |= flag::RIM_NO_SAMPLE;
    }
    (r.0, extra)
}

/// Everything sampled for one in-box column of a chunk.
struct ColOut {
    alt: f32,
    riverless_alt: f32,
    water_level: f32,
    warp_factor: f32,
    top_kind: u8,
    sum: format::ColSummary,
    runs: Vec<(u8, u16)>,
}

/// Local `[lx0, lx1) x [ly0, ly1)` of the box inside chunk `key`.
pub(crate) fn chunk_local_range(b: Box2, key: Vec2<i32>) -> (i32, i32, i32, i32) {
    let base = key * CHUNK;
    (
        (b.x0 - base.x).clamp(0, CHUNK),
        (b.x1 - base.x).clamp(0, CHUNK),
        (b.y0 - base.y).clamp(0, CHUNK),
        (b.y1 - base.y).clamp(0, CHUNK),
    )
}

/// The column sampler's `[alt, riverless_alt, water_level, warp_factor]` for
/// every in-box column of chunk `key`, row-major (NaN where it returned
/// nothing).
///
/// `World::generate_chunk` samples these same columns internally and does not
/// hand them back (it returns only the chunk and its supplement), so this is a
/// second sampling pass; `ChunkTimes::sample_ns` measures its cost. The client
/// path calls it too: the client never receives the sampler's output, but the
/// probe's in-process world gives the surface that `classify_at` needs.
pub(crate) fn sample_chunk_floats(p: &Probe, b: Box2, key: Vec2<i32>) -> Vec<[f32; 4]> {
    let ir = p.index.as_index_ref();
    let cg = p.world.sample_columns();
    let base = key * CHUNK;
    let (lx0, lx1, ly0, ly1) = chunk_local_range(b, key);
    let mut out = Vec::with_capacity(((lx1 - lx0) * (ly1 - ly0)).max(0) as usize);
    for ly in ly0..ly1 {
        for lx in lx0..lx1 {
            let w = base + Vec2::new(lx, ly);
            out.push(cg.get((w, ir, None)).map_or([f32::NAN; 4], |s| {
                [s.alt, s.riverless_alt, s.water_level, s.warp_factor]
            }));
        }
    }
    out
}

/// Per-chunk timings, in nanoseconds of one worker thread.
#[derive(Clone, Copy, Default)]
struct ChunkTimes {
    gen_ns: u64,
    sample_ns: u64,
    runs_ns: u64,
}

fn chunk_columns(
    p: &Probe,
    b: Box2,
    key: Vec2<i32>,
    zrange: (i32, i32),
    keep_sprites: bool,
) -> Res<(Vec<ColOut>, ChunkTimes)> {
    let (zmin, zmax) = zrange;
    let ir = p.index.as_index_ref();
    let t = std::time::Instant::now();
    let ch = p
        .world
        .generate_chunk(ir, key, None, || false, None, None)
        .map_err(|()| format!("generate_chunk failed at {key}"))?
        .0;
    let gen_ns = t.elapsed().as_nanos() as u64;
    let t = std::time::Instant::now();
    let floats = sample_chunk_floats(p, b, key);
    let sample_ns = t.elapsed().as_nanos() as u64;
    let t = std::time::Instant::now();
    let (lx0, lx1, ly0, ly1) = chunk_local_range(b, key);
    // Row-major (y, then x) over the in-box part of the chunk.
    let mut out = Vec::with_capacity(floats.len());
    let mut fl = floats.into_iter();
    for ly in ly0..ly1 {
        for lx in lx0..lx1 {
            let rel = Vec2::new(lx, ly);
            let [alt, riverless_alt, water_level, warp_factor] = fl.next().unwrap_or([f32::NAN; 4]);
            let (runs, extra) =
                column_runs_ex(&ch, rel, zmin, zmax, keep_sprites, surface_cap(alt));
            let mut sum = format::summarize(zmin, &runs);
            sum.flags |= extra;
            let top_kind = if sum.ground_top == format::NO_Z {
                255
            } else {
                ch.get(rel.with_z(i32::from(sum.ground_top)))
                    .map_or(255, |bl| bl.kind() as u8)
            };
            out.push(ColOut {
                alt,
                riverless_alt,
                water_level,
                warp_factor,
                top_kind,
                sum,
                runs,
            });
        }
    }
    let runs_ns = t.elapsed().as_nanos() as u64;
    Ok((out, ChunkTimes {
        gen_ns,
        sample_ns,
        runs_ns,
    }))
}

pub struct DumpOpts {
    pub bx: Box2,
    pub zmin: Option<i32>,
    pub zmax: Option<i32>,
    pub site_margin: i32,
    /// Record sprite blocks as `SPRITE` instead of air (not reproducible).
    pub keep_sprites: bool,
    /// Skip the pre-flight memory check ([`check_memory`]).
    pub force: bool,
}

pub struct DumpStats {
    pub chunks: usize,
    pub columns: usize,
    pub gen_secs: f32,
    /// Summed worker-thread seconds spent in `World::generate_chunk`.
    pub cpu_gen_secs: f32,
    /// ... in the second column-sampler pass ([`sample_chunk_floats`]).
    pub cpu_sample_secs: f32,
    /// ... classifying blocks into runs.
    pub cpu_runs_secs: f32,
}

/// Resolve `--zmin/--zmax` (falling back to the automatic range) and validate
/// them against what a dump can hold: z is stored as `i16`, `-32768` is the
/// "none" sentinel, and a column is at most `MAX_HEIGHT` blocks.
pub fn resolve_z_range(zmin: Option<i32>, zmax: Option<i32>, auto: (i32, i32)) -> Res<(i32, i32)> {
    let zmin = zmin.unwrap_or_else(|| auto.0.max(format::Z_MIN_ALLOWED));
    let zmax = zmax.unwrap_or_else(|| auto.1.min(format::Z_MAX_ALLOWED));
    format::check_z_range(zmin, zmax)?;
    Ok((zmin, zmax))
}

/// [`resolve_z_range`] that evaluates the (expensive) automatic range only
/// when `--zmin`/`--zmax` do not both fix it.
pub fn resolve_z_lazy(
    zmin: Option<i32>,
    zmax: Option<i32>,
    auto: impl FnOnce() -> (i32, i32),
) -> Res<(i32, i32)> {
    let auto = if zmin.is_some() && zmax.is_some() {
        (0, 0)
    } else {
        auto()
    };
    resolve_z_range(zmin, zmax, auto)
}

/// Peak memory (bytes) the fast path needs for `columns` columns: the dump's
/// column arrays plus run storage and the write path's compressed sections.
/// Calibrated against a measured peak RSS of 2.43 GB for 25 M columns over a
/// 1609 block z range (97 bytes per column; boxes with a tight z range need
/// far less, so this errs on the safe side).
pub const FAST_BYTES_PER_COLUMN: u64 = 100;
/// Same for `client-dump`, which keeps a `Vec` of runs per column until the
/// stream finishes (not measured; derived from the fast path plus the
/// per-column allocations).
pub const CLIENT_BYTES_PER_COLUMN: u64 = 160;
/// Refuse boxes estimated above this unless `--force` is given.
pub const MEM_LIMIT_BYTES: u64 = 8 << 30;

/// Pre-flight check: refuse a box whose estimated peak memory exceeds
/// [`MEM_LIMIT_BYTES`] unless `force`.
pub fn check_memory(columns: u64, bytes_per_column: u64, force: bool) -> Res<()> {
    let est = columns.saturating_mul(bytes_per_column);
    if est > MEM_LIMIT_BYTES && !force {
        return Err(format!(
            "{columns} columns need an estimated {:.1} GB of memory (limit {:.0} GB); shrink the \
             box, or pass --force to try anyway",
            est as f64 / 1e9,
            MEM_LIMIT_BYTES as f64 / 1e9
        )
        .into());
    }
    Ok(())
}

/// Fast path: generate every chunk of the box with `World::generate_chunk` and
/// record every 1 m column.
pub fn dump(
    p: &Probe,
    o: &DumpOpts,
    progress: &(dyn Fn(usize, usize) + Sync),
) -> Res<(Dump, DumpStats)> {
    let t0 = std::time::Instant::now();
    let b = o.bx;
    let size = world_size(p.world.sim());
    if b.x0 < 0 || b.y0 < 0 || b.x1 > size.x || b.y1 > size.y {
        return Err(format!(
            "box {},{},{},{} outside the world (0,0)..({},{})",
            b.x0, b.y0, b.x1, b.y1, size.x, size.y
        )
        .into());
    }
    let (c0, c1) = b.chunk_range();
    let ncx = (c1.x - c0.x + 1) as usize;
    let ncy = (c1.y - c0.y + 1) as usize;
    let n = b.nx() * b.ny();
    // Refuse oversized boxes before the automatic z range samples the box.
    check_memory(n as u64, FAST_BYTES_PER_COLUMN, o.force)?;
    let (zmin, zmax) = resolve_z_lazy(o.zmin, o.zmax, || auto_z_range_for(p, b))?;

    let mut alt = Vec::with_capacity(n);
    let mut riverless_alt = Vec::with_capacity(n);
    let mut water_level = Vec::with_capacity(n);
    let mut warp_factor = Vec::with_capacity(n);
    let mut top_kind = Vec::with_capacity(n);
    let mut flags = Vec::with_capacity(n);
    let mut ground_top = Vec::with_capacity(n);
    let mut water_top = Vec::with_capacity(n);
    let mut liquid_depth = Vec::with_capacity(n);
    let mut run_counts = Vec::with_capacity(n);
    let mut run_class = Vec::new();
    let mut run_len = Vec::new();
    let mut clipped = 0u64;
    let mut above = 0u64;
    let (mut lava, mut rim) = (0u64, 0u64);
    let mut times = (0u64, 0u64, 0u64);

    // Process the box in bands of chunk rows so memory stays bounded while the
    // pool still has plenty of independent chunks to chew on.
    let band = (256 / ncx).max(1);
    let mut done = 0usize;
    let mut row = 0usize;
    while row < ncy {
        let rows = band.min(ncy - row);
        let keys: Vec<Vec2<i32>> = (0..rows)
            .flat_map(|r| (0..ncx).map(move |cx| (r, cx)))
            .map(|(r, cx)| Vec2::new(c0.x + cx as i32, c0.y + (row + r) as i32))
            .collect();
        let results: Vec<Res<(Vec<ColOut>, ChunkTimes)>> = p.pool.install(|| {
            keys.par_iter()
                .map(|&k| chunk_columns(p, b, k, (zmin, zmax), o.keep_sprites))
                .collect()
        });
        let mut cols: Vec<Vec<ColOut>> = Vec::with_capacity(results.len());
        for r in results {
            let (c, t) = r?;
            times.0 += t.gen_ns;
            times.1 += t.sample_ns;
            times.2 += t.runs_ns;
            cols.push(c);
        }
        // Assemble the box rows covered by each chunk row of this band.
        for r in 0..rows {
            let cy = c0.y + (row + r) as i32;
            let (ly0, ly1) = (
                (b.y0 - cy * CHUNK).clamp(0, CHUNK),
                (b.y1 - cy * CHUNK).clamp(0, CHUNK),
            );
            for ly in 0..(ly1 - ly0) {
                for cx in 0..ncx {
                    let key_x = c0.x + cx as i32;
                    let (lx0, lx1) = (
                        (b.x0 - key_x * CHUNK).clamp(0, CHUNK),
                        (b.x1 - key_x * CHUNK).clamp(0, CHUNK),
                    );
                    let w = (lx1 - lx0) as usize;
                    let chunk_cols = &cols[r * ncx + cx];
                    for c in &chunk_cols[ly as usize * w..(ly as usize + 1) * w] {
                        alt.push(c.alt);
                        riverless_alt.push(c.riverless_alt);
                        water_level.push(c.water_level);
                        warp_factor.push(c.warp_factor);
                        top_kind.push(c.top_kind);
                        flags.push(c.sum.flags);
                        clipped += u64::from(c.sum.flags & flag::CLIPPED_TOP != 0);
                        above += u64::from(c.sum.flags & flag::STRUCTURE_ABOVE_GROUND != 0);
                        lava += u64::from(c.sum.flags & flag::LAVA != 0);
                        rim += u64::from(c.sum.flags & flag::RIM_NO_SAMPLE != 0);
                        ground_top.push(c.sum.ground_top);
                        water_top.push(c.sum.water_top);
                        liquid_depth.push(c.sum.liquid_depth);
                        run_counts.push(c.runs.len() as u16);
                        for &(cl, len) in &c.runs {
                            run_class.push(cl);
                            run_len.push(len);
                        }
                    }
                }
            }
        }
        row += rows;
        done += keys.len();
        progress(done, ncx * ncy);
    }
    let gen_secs = t0.elapsed().as_secs_f32();

    let ir = p.index.as_index_ref();
    let sim_csv = sim_table(p, c0 - Vec2::broadcast(2), c1 + Vec2::broadcast(2));
    let mut sites = world_sites(&ir, b, o.site_margin);
    sites.extend(authored_points(
        &p.assets_root,
        world_size(p.world.sim()),
        b,
        o.site_margin,
    ));

    let header = Header {
        format: format::FORMAT_NAME.into(),
        format_rev: format::FORMAT_REV,
        flags_defined: format::FLAGS_DEFINED,
        box_xy: [b.x0, b.y0, b.x1, b.y1],
        zmin,
        zmax,
        nx: b.nx() as u32,
        ny: b.ny() as u32,
        seed: p.seed,
        path: "fast".into(),
        calendar: None,
        engine_commit: env!("TPROBE_ENGINE_COMMIT").to_string(),
        assets: asset_hashes(&p.assets_root),
        class_codes: Header::class_codes(),
        block_kinds: block_kind_codes(),
        stats: [
            ("columns".to_string(), n as u64),
            ("chunks".to_string(), (ncx * ncy) as u64),
            ("clipped_top_columns".to_string(), clipped),
            ("structure_above_ground_columns".to_string(), above),
            ("lava_columns".to_string(), lava),
            ("rim_no_sample_columns".to_string(), rim),
            ("sprites_kept".to_string(), u64::from(o.keep_sprites)),
            ("sites".to_string(), sites.len() as u64),
        ]
        .into_iter()
        .collect(),
        client: None,
        sections: vec![],
    };
    Ok((
        Dump {
            header,
            alt,
            riverless_alt,
            water_level,
            warp_factor,
            top_kind,
            flags,
            ground_top,
            water_top,
            liquid_depth,
            run_counts,
            run_class,
            run_len,
            sim_csv,
            sites,
        },
        DumpStats {
            chunks: ncx * ncy,
            columns: n,
            gen_secs,
            cpu_gen_secs: times.0 as f32 / 1e9,
            cpu_sample_secs: times.1 as f32 / 1e9,
            cpu_runs_secs: times.2 as f32 / 1e9,
        },
    ))
}

/// Width/depth of a river chunk's cross-section. The engine's `RiverKind` is
/// not nameable outside `xindeler-world`, so read it from its `Debug` form
/// (`River { cross_section: Vec2 { x: 64.0, y: 8.0 } }`).
fn cross_section(dbg: &str) -> Option<(f32, f32)> {
    let after = |key: &str| -> Option<f32> {
        let i = dbg.find(key)? + key.len();
        let rest = &dbg[i..];
        let end = rest.find([',', ' ', '}']).unwrap_or(rest.len());
        rest[..end].parse().ok()
    };
    Some((after("x: ")?, after("y: ")?))
}

pub const SIM_HEADER: &str = "cx;cy;wx;wy;alt;water_alt;basement;chaos;river_kind;cross_w;cross_h;\
                              vel_x;vel_y;vel_z;rockiness;cliff_height;is_path;humidity;temp;\
                              tree_density;flux;underwater";

/// Per-chunk sim table for the inclusive chunk range `[c0, c1]` (clamped to
/// the map). Velocity is read from the column sampler at the chunk centre, as
/// the chunk's own river velocity is not public.
pub fn sim_table(p: &Probe, c0: Vec2<i32>, c1: Vec2<i32>) -> String {
    use std::fmt::Write as _;
    let sim = p.world.sim();
    let ir = p.index.as_index_ref();
    let cg = p.world.sample_columns();
    let mut s = String::from(SIM_HEADER);
    s.push('\n');
    for cy in c0.y..=c1.y {
        for cx in c0.x..=c1.x {
            let Some(c) = sim.get(Vec2::new(cx, cy)) else {
                continue;
            };
            let w = Vec2::new(cx, cy) * CHUNK + Vec2::broadcast(CHUNK / 2);
            let vel = cg
                .get((w, ir, None))
                .map_or([0.0; 3], |s| [s.water_vel.x, s.water_vel.y, s.water_vel.z]);
            let (kind, cw, ch) = if c.river.is_ocean() {
                ("ocean", 0.0, 0.0)
            } else if c.river.is_lake() {
                ("lake", 0.0, 0.0)
            } else if c.river.is_river() {
                let (w, h) = cross_section(&format!("{:?}", c.river.river_kind))
                    .unwrap_or((f32::NAN, f32::NAN));
                ("river", w, h)
            } else {
                ("none", 0.0, 0.0)
            };
            let _ = writeln!(
                s,
                "{cx};{cy};{};{};{:.3};{:.3};{:.3};{:.3};{kind};{cw:.2};{ch:.2};{:.3};{:.3};{:.3};\
                 {:.3};{:.2};{};{:.3};{:.2};{:.3};{:.3};{}",
                cx * CHUNK,
                cy * CHUNK,
                c.alt,
                c.water_alt,
                c.basement,
                c.chaos,
                vel[0],
                vel[1],
                vel[2],
                c.rockiness,
                c.cliff_height,
                u8::from(c.path.0.is_way()),
                c.humidity,
                c.temp,
                c.tree_density,
                c.flux,
                u8::from(c.is_underwater()),
            );
        }
    }
    s
}

/// Whether the point lies inside the box grown by `margin` on every side,
/// **boundary included** (closed, like [`segment_near_box`], so a point and a
/// segment end at the same place are judged alike; i64 arithmetic: boxes and
/// margins near the i32 limits cannot wrap).
fn near_box(x: i32, y: i32, b: Box2, margin: i32) -> bool {
    let (x, y, m) = (i64::from(x), i64::from(y), i64::from(margin));
    x >= i64::from(b.x0) - m
        && x <= i64::from(b.x1) + m
        && y >= i64::from(b.y0) - m
        && y <= i64::from(b.y1) + m
}

/// Whether the segment `a`-`e` meets the box grown by `margin` (closed
/// rectangle; Liang-Barsky clipping, exact, no sampling).
fn segment_near_box(a: (f64, f64), e: (f64, f64), b: Box2, margin: i32) -> bool {
    let m = f64::from(margin);
    let (xmin, xmax) = (f64::from(b.x0) - m, f64::from(b.x1) + m);
    let (ymin, ymax) = (f64::from(b.y0) - m, f64::from(b.y1) + m);
    let (dx, dy) = (e.0 - a.0, e.1 - a.1);
    let (mut t0, mut t1) = (0.0f64, 1.0f64);
    for (p, q) in [
        (-dx, a.0 - xmin),
        (dx, xmax - a.0),
        (-dy, a.1 - ymin),
        (dy, ymax - a.1),
    ] {
        if p == 0.0 {
            if q < 0.0 {
                return false;
            }
        } else {
            let r = q / p;
            if p < 0.0 {
                t0 = t0.max(r);
            } else {
                t1 = t1.min(r);
            }
            if t0 > t1 {
                return false;
            }
        }
    }
    true
}

/// Point of the segment `a`-`e` closest to `c`.
fn closest_on_segment(a: (f64, f64), e: (f64, f64), c: (f64, f64)) -> (f64, f64) {
    let (dx, dy) = (e.0 - a.0, e.1 - a.1);
    let l2 = dx * dx + dy * dy;
    let t = if l2 == 0.0 {
        0.0
    } else {
        (((c.0 - a.0) * dx + (c.1 - a.1) * dy) / l2).clamp(0.0, 1.0)
    };
    (a.0 + dx * t, a.1 + dy * t)
}

/// Sites in the generated world index whose area comes within `margin` of the
/// box: the origin is tested against the box grown by `margin` plus the
/// site's own radius.
pub fn world_sites(ir: &world::IndexRef, b: Box2, margin: i32) -> Vec<SiteRec> {
    let mut out = Vec::new();
    for (_, site) in ir.sites.iter() {
        let o = site.origin;
        let radius = site.radius();
        let reach = margin.saturating_add(if radius.is_finite() {
            radius.ceil().clamp(0.0, 1.0e6) as i32
        } else {
            0
        });
        if near_box(o.x, o.y, b, reach) {
            out.push(SiteRec {
                source: "world_site".into(),
                id: None,
                name: site.name().map(str::to_owned),
                kind: site.kind.as_ref().map(|k| format!("{k:?}")),
                wx: o.x,
                wy: o.y,
                radius: Some(radius),
            });
        }
    }
    out.sort_by(|a, b| (a.wx, a.wy, &a.name).cmp(&(b.wx, b.wy, &b.name)));
    out
}

/// One authored feature in world metres: a point (one vertex), or a
/// polyline / wall / bridge (several vertices; the segments between them are
/// part of the feature, route vertices are kilometres apart).
#[derive(Clone, Debug)]
pub struct AuthoredFeature {
    /// The `cromatolis_v0_*.ron` file it came from.
    pub source: String,
    pub id: Option<String>,
    pub name: Option<String>,
    pub kind: Option<String>,
    pub pts: Vec<(f64, f64)>,
    /// Extent around the vertices (only the aerial citadel has one).
    pub radius: Option<f32>,
    /// An authored area `(x0, y0, x1, y1)` in world metres (authored raster
    /// regions): near a box when the two rectangles come within reach, not
    /// only its outline.
    pub area: Option<(f64, f64, f64, f64)>,
}

/// How a file's `x`/`y` fields map to world metres.
#[derive(Clone, Copy)]
enum Space {
    /// `normalized_map_xy_top_left*`: x, y in 0..=1, origin top-left.
    Normalized,
    /// `source_pixels_xy_top_left_origin`: pixels of a `w` x `h` source map,
    /// normalised as `px / (w - 1)` exactly like the engine's fortification
    /// loader (`normalize_point` in `world/src/civ/mod.rs`).
    Pixels { w: f64, h: f64 },
}

impl Space {
    /// World metres of the pair, or `None` when it is outside the map.
    fn to_world(self, x: f64, y: f64, size: Vec2<i32>) -> Option<(f64, f64)> {
        let (u, v) = match self {
            Self::Normalized => (x, y),
            Self::Pixels { w, h } => (x / (w - 1.0).max(1.0), y / (h - 1.0).max(1.0)),
        };
        ((0.0..=1.0).contains(&u) && (0.0..=1.0).contains(&v))
            .then(|| (u * f64::from(size.x), (1.0 - v) * f64::from(size.y)))
    }
}

/// `cromatolis_v0_*.ron` files that declare no `coordinate_space` because they
/// carry no map positions (climate tables, ground-cover rules, interior
/// graphs...). Any other such file is an error: a new file with positions
/// must not be silently ignored by the emptiness guard.
pub const NO_POSITION_FILES: &[&str] = &[
    "cromatolis_v0_alpine.ron",
    "cromatolis_v0_aquatic_ecology.ron",
    "cromatolis_v0_climate.ron",
    "cromatolis_v0_ground_cover.ron",
    "cromatolis_v0_ground_substrate_zones.ron",
    "cromatolis_v0_interior_graphs.ron",
    "cromatolis_v0_interior_places.ron",
    "cromatolis_v0_map_ecology.ron",
    "cromatolis_v0_procedural_layers.ron",
    // Ward rectangles in each settlement's own frame; the settlements
    // themselves are positioned (and guarded) through `cromatolis_v0_sites.ron`.
    "cromatolis_v0_settlement_layouts.ron",
    "cromatolis_v0_tree_candidate_policy.ron",
];

/// Slack (m) added to every authored feature's reach. The engine places
/// authored points on chunk centres of a `(chunks - 1)` grid
/// (`AuthoredMapPoint::to_chunk_pos`), while this tool maps `u * size`
/// linearly; the two differ by up to 64 m (test
/// `ec_f01_pixel_mapping_matches_the_engine_within_the_snap_slack`), so the
/// guard widens its reach by that much.
pub const ENGINE_SNAP_SLACK_M: i32 = 64;

/// Every authored feature of the `cromatolis_v0_*.ron` files, in world metres.
///
/// Handled coordinate spaces: `normalized_map_xy_top_left*`,
/// `source_pixels_xy_top_left_origin` (walls and gates, converted through the
/// file's own `source_map`), `citadel_relative_meters` (the aerial citadel's
/// absolute `center` and `max_radius_m`) and `inherits_landmark_center` (no
/// positions of its own). A file that declares any other coordinate space, or
/// declares one and cannot be parsed, is an `Err`: a guard that silently
/// skipped it could call a box empty while something authored stands there.
pub fn authored_features(root: &Path, size: Vec2<i32>) -> Result<Vec<AuthoredFeature>, String> {
    let rd = std::fs::read_dir(root.join("world/map"))
        .map_err(|e| format!("cannot read {}/world/map: {e}", root.display()))?;
    let mut files: Vec<_> = rd
        .flatten()
        .filter(|e| {
            let n = e.file_name().to_string_lossy().into_owned();
            n.starts_with("cromatolis_v0_") && n.ends_with(".ron")
        })
        .collect();
    files.sort_by_key(|e| e.file_name());
    let mut out = Vec::new();
    for e in files {
        let name = e.file_name().to_string_lossy().into_owned();
        let text = std::fs::read_to_string(e.path()).map_err(|e| format!("{name}: {e}"))?;
        // Authored raster regions (the engine's authored water layer) are
        // authored areas in wpos: every region box is a feature.
        if name == "cromatolis_v0_authored_rasters.ron" {
            let m: world::authored_raster::Manifest =
                ron::from_str(&text).map_err(|err| format!("{name}: does not parse: {err}"))?;
            for r in m.regions {
                let (x0, y0) = (f64::from(r.min.0), f64::from(r.min.1));
                let (x1, y1) = (f64::from(r.max.0), f64::from(r.max.1));
                out.push(AuthoredFeature {
                    source: name.clone(),
                    id: Some(r.id),
                    name: None,
                    kind: Some("authored_raster_region".into()),
                    pts: vec![(x0, y0), (x1, y0), (x1, y1), (x0, y1), (x0, y0)],
                    radius: None,
                    area: Some((x0, y0, x1, y1)),
                });
            }
            continue;
        }
        let v = match ron::from_str::<ron::Value>(&text) {
            Ok(v) => v,
            Err(err)
                if text.contains("coordinate_space")
                    || !NO_POSITION_FILES.contains(&name.as_str()) =>
            {
                return Err(format!("{name}: does not parse: {err}"));
            },
            Err(_) => continue, // an allowlisted file this tool never needs to parse
        };
        let ron::Value::Map(top) = &v else {
            return Err(format!("{name}: top level is not a struct"));
        };
        let Some(space) = field(top, "coordinate_space").and_then(str_of) else {
            if NO_POSITION_FILES.contains(&name.as_str()) {
                continue;
            }
            return Err(format!(
                "{name}: no coordinate_space and not in NO_POSITION_FILES; add it to the \
                 allowlist (it has no positions) or teach authored_features() its layout"
            ));
        };
        let space = match space.as_str() {
            s if s.starts_with("normalized_map_xy_top_left") => Space::Normalized,
            "source_pixels_xy_top_left_origin" => {
                let dims = field(top, "source_map").and_then(|m| match m {
                    ron::Value::Map(m) => Some((
                        field(m, "width_px").and_then(num)?,
                        field(m, "height_px").and_then(num)?,
                    )),
                    _ => None,
                });
                let Some((w, h)) = dims else {
                    return Err(format!(
                        "{name}: source-pixel file without source_map dimensions"
                    ));
                };
                Space::Pixels { w, h }
            },
            "citadel_relative_meters" => {
                if let Some(f) = citadel_feature(top, &name) {
                    out.push(f);
                }
                continue;
            },
            "inherits_landmark_center" => continue,
            other => {
                return Err(format!(
                    "{name}: unhandled coordinate space {other:?}; teach authored_features() \
                     about it before trusting an emptiness check"
                ));
            },
        };
        collect_features(&v, &name, space, size, &Ctx::default(), &mut out);
    }
    Ok(out)
}

/// The aerial citadel: `center: (x, y)` is absolute world metres, the rest
/// is relative to it; `max_radius_m` bounds the whole structure.
fn citadel_feature(top: &ron::Map, file: &str) -> Option<AuthoredFeature> {
    let ron::Value::Seq(c) = field(top, "center")? else {
        return None;
    };
    let [x, y] = &c[..] else { return None };
    Some(AuthoredFeature {
        source: file.to_string(),
        id: Some("aerial_citadel".into()),
        name: None,
        kind: Some("aerial_citadel".into()),
        pts: vec![(num(x)?, num(y)?)],
        radius: field(top, "max_radius_m").and_then(num).map(|r| r as f32),
        area: None,
    })
}

#[derive(Default, Clone)]
struct Ctx {
    id: Option<String>,
    name: Option<String>,
    kind: Option<String>,
}

fn xy(v: &ron::Value) -> Option<(f64, f64)> {
    match v {
        ron::Value::Map(m) => Some((field(m, "x").and_then(num)?, field(m, "y").and_then(num)?)),
        _ => None,
    }
}

fn collect_features(
    v: &ron::Value,
    file: &str,
    space: Space,
    size: Vec2<i32>,
    ctx: &Ctx,
    out: &mut Vec<AuthoredFeature>,
) {
    match v {
        ron::Value::Map(m) => {
            let pick = |k: &[&str], old: &Option<String>| {
                k.iter()
                    .find_map(|k| field(m, k).and_then(str_of))
                    .or_else(|| old.clone())
            };
            let ctx = Ctx {
                id: pick(&["id"], &ctx.id),
                name: pick(&["name"], &ctx.name),
                kind: pick(&["kind", "category"], &ctx.kind),
            };
            let feature = |pts: Vec<(f64, f64)>, kind: Option<String>| AuthoredFeature {
                source: file.to_string(),
                id: ctx.id.clone(),
                name: ctx.name.clone(),
                kind,
                pts,
                radius: None,
                area: None,
            };
            let world = |p: (f64, f64)| space.to_world(p.0, p.1, size);
            // A polyline: `points: [(x:, y:), ...]` (routes, maritime routes).
            if let Some(ron::Value::Seq(pts)) = field(m, "points") {
                let pts: Vec<_> = pts.iter().filter_map(xy).filter_map(world).collect();
                if !pts.is_empty() {
                    let kind = ctx
                        .kind
                        .clone()
                        .or_else(|| (pts.len() > 1).then(|| "polyline".into()));
                    out.push(feature(pts, kind));
                    return;
                }
            }
            // A wall or bridge: `start` and `end` points, the span between
            // them is the feature. Other children (gates) are visited too.
            let span = match (field(m, "start").and_then(xy), field(m, "end").and_then(xy)) {
                (Some(a), Some(e)) => world(a).zip(world(e)),
                _ => None,
            };
            if let Some((a, e)) = span {
                out.push(feature(
                    vec![a, e],
                    ctx.kind.clone().or_else(|| Some("span".into())),
                ));
                for (k, child) in m.iter() {
                    if !matches!(k, ron::Value::String(s) if s == "start" || s == "end") {
                        collect_features(child, file, space, size, &ctx, out);
                    }
                }
                return;
            }
            if let Some(p) = xy(v).and_then(world) {
                out.push(feature(vec![p], ctx.kind.clone()));
                return;
            }
            for (_, child) in m.iter() {
                collect_features(child, file, space, size, &ctx, out);
            }
        },
        ron::Value::Seq(items) => {
            for item in items {
                collect_features(item, file, space, size, ctx, out);
            }
        },
        ron::Value::Option(Some(inner)) => collect_features(inner, file, space, size, ctx, out),
        _ => {},
    }
}

/// Authored features within `margin` of the box (the box grown by `margin`
/// on every side): points by position, polylines / walls / bridges by their
/// **segments**, so a route whose vertices are kilometres away but whose
/// span crosses the box is reported. Each feature yields one record, placed
/// at the feature's point closest to the box centre. A point feature's
/// `radius` extends the reach. Errors as [`authored_features`].
pub fn authored_near(
    root: &Path,
    size: Vec2<i32>,
    b: Box2,
    margin: i32,
) -> Result<Vec<SiteRec>, String> {
    let centre = (
        (f64::from(b.x0) + f64::from(b.x1)) / 2.0,
        (f64::from(b.y0) + f64::from(b.y1)) / 2.0,
    );
    let mut out = Vec::new();
    for f in authored_features(root, size)? {
        let reach = margin
            .saturating_add(ENGINE_SNAP_SLACK_M)
            .saturating_add(f.radius.map_or(0, |r| r.ceil().clamp(0.0, 1.0e6) as i32));
        let best = if let Some((x0, y0, x1, y1)) = f.area {
            let m = f64::from(reach);
            let hit = x0 <= f64::from(b.x1) + m
                && x1 >= f64::from(b.x0) - m
                && y0 <= f64::from(b.y1) + m
                && y1 >= f64::from(b.y0) - m;
            hit.then(|| (centre.0.clamp(x0, x1), centre.1.clamp(y0, y1)))
        } else if let [p] = f.pts[..] {
            let (x, y) = (p.0.round() as i32, p.1.round() as i32);
            near_box(x, y, b, reach).then_some(p)
        } else {
            f.pts
                .windows(2)
                .filter(|w| segment_near_box(w[0], w[1], b, reach))
                .map(|w| closest_on_segment(w[0], w[1], centre))
                .min_by(|p, q| {
                    let d = |p: &(f64, f64)| (p.0 - centre.0).hypot(p.1 - centre.1);
                    d(p).total_cmp(&d(q))
                })
        };
        if let Some((x, y)) = best {
            out.push(SiteRec {
                source: f.source,
                id: f.id,
                name: f.name,
                kind: f.kind,
                wx: x.round() as i32,
                wy: y.round() as i32,
                radius: f.radius,
            });
        }
    }
    out.sort_by(|a, b| (&a.source, a.wx, a.wy, &a.id).cmp(&(&b.source, b.wx, b.wy, &b.id)));
    Ok(out)
}

/// [`authored_near`] for the dump's site list: a file it cannot interpret is
/// reported on stderr instead of failing the whole dump (use `sites-near`,
/// which is strict, for emptiness checks).
pub fn authored_points(root: &Path, size: Vec2<i32>, b: Box2, margin: i32) -> Vec<SiteRec> {
    authored_near(root, size, b, margin).unwrap_or_else(|e| {
        eprintln!("warning: authored points skipped: {e}");
        Vec::new()
    })
}

#[cfg(test)]
/// Every authored feature as point records, unfiltered: one per point
/// feature, one per vertex of a polyline / wall / bridge. (Lenient like
/// [`authored_points`].)
pub fn all_authored_points(root: &Path, size: Vec2<i32>, out: &mut Vec<SiteRec>) {
    match authored_features(root, size) {
        Ok(fs) => {
            for f in fs {
                for &(x, y) in &f.pts {
                    out.push(SiteRec {
                        source: f.source.clone(),
                        id: f.id.clone(),
                        name: f.name.clone(),
                        kind: f.kind.clone(),
                        wx: x.round() as i32,
                        wy: y.round() as i32,
                        radius: f.radius,
                    });
                }
            }
        },
        Err(e) => eprintln!("warning: authored points skipped: {e}"),
    }
}

fn str_of(v: &ron::Value) -> Option<String> {
    match v {
        ron::Value::String(s) => Some(s.clone()),
        _ => None,
    }
}

fn field<'a>(m: &'a ron::Map, key: &str) -> Option<&'a ron::Value> {
    m.iter()
        .find(|(k, _)| matches!(k, ron::Value::String(s) if s == key))
        .map(|(_, v)| v)
}

#[cfg(test)]
fn coordinate_space_is_normalized(v: &ron::Value) -> bool {
    matches!(v, ron::Value::Map(m)
        if field(m, "coordinate_space")
            .and_then(str_of)
            .is_some_and(|s| s.starts_with("normalized_map_xy_top_left")))
}

fn num(v: &ron::Value) -> Option<f64> {
    match v {
        ron::Value::Number(n) => Some(n.into_f64()),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_cross_section_debug() {
        let d = "Some(River { cross_section: Vec2 { x: 64.0, y: 8.5 } })";
        assert_eq!(cross_section(d), Some((64.0, 8.5)));
        assert_eq!(cross_section("Some(Ocean)"), None);
    }

    #[test]
    fn near_box_and_segments_agree_on_the_boundary() {
        let b = Box2 {
            x0: 100,
            y0: 100,
            x1: 200,
            y1: 200,
        };
        // Exactly `margin` away: near for a point and for a degenerate or
        // touching segment alike.
        assert!(near_box(50, 150, b, 50));
        assert!(segment_near_box((50.0, 150.0), (50.0, 150.0), b, 50));
        assert!(segment_near_box((0.0, 150.0), (50.0, 150.0), b, 50));
        assert!(!near_box(49, 150, b, 50));
        assert!(!segment_near_box((0.0, 150.0), (49.0, 150.0), b, 50));
    }

    #[test]
    fn auto_z_is_only_evaluated_when_needed() {
        let calls = std::cell::Cell::new(0);
        let auto = || {
            calls.set(calls.get() + 1);
            (10, 90)
        };
        assert_eq!(resolve_z_lazy(Some(0), Some(50), auto).unwrap(), (0, 50));
        assert_eq!(calls.get(), 0, "both given: no automatic range");
        assert_eq!(resolve_z_lazy(None, Some(50), auto).unwrap(), (10, 50));
        assert_eq!(resolve_z_lazy(Some(0), None, auto).unwrap(), (0, 90));
        assert_eq!(calls.get(), 2);
        // Invalid explicit ranges still error without the automatic one.
        assert!(resolve_z_lazy(Some(5), Some(5), auto).is_err());
        assert_eq!(calls.get(), 2);
    }

    #[test]
    fn box_parse_and_chunks() {
        let b = Box2::parse("0,0,33,64").unwrap();
        assert_eq!((b.nx(), b.ny()), (33, 64));
        let (c0, c1) = b.chunk_range();
        assert_eq!((c0, c1), (Vec2::new(0, 0), Vec2::new(1, 1)));
        assert!(Box2::parse("5,5,5,9").is_err());
        assert!(Box2::parse("1,2,3").is_err());
    }

    /// `cross_section` reads the engine's `Debug` output of `RiverKind`, so a
    /// format change would silently turn every river's width/depth into NaN.
    /// Returns the number of river rows checked.
    fn assert_no_nan_cross_sections(csv: &str) -> usize {
        let mut lines = csv.lines();
        let header: Vec<&str> = lines.next().unwrap().split(';').collect();
        let col = |n: &str| header.iter().position(|h| *h == n).unwrap();
        let (kind, w, h) = (col("river_kind"), col("cross_w"), col("cross_h"));
        let mut rivers = 0;
        for line in lines {
            let f: Vec<&str> = line.split(';').collect();
            assert!(
                !f[w].contains("NaN") && !f[h].contains("NaN"),
                "NaN cross-section: {line}"
            );
            if f[kind] == "river" {
                rivers += 1;
                let (cw, ch): (f32, f32) = (f[w].parse().unwrap(), f[h].parse().unwrap());
                assert!(
                    cw > 0.0 && ch > 0.0,
                    "non-positive river cross-section: {line}"
                );
            }
        }
        rivers
    }

    /// Whole-world sim table: every river chunk has a finite, positive
    /// cross-section (the `Debug` parse of `RiverKind` still works).
    ///
    /// `VELOREN_ASSETS=/path/to/assets cargo test -p xindeler-terrain-probe
    /// -- --ignored real_sim_table`
    #[test]
    #[ignore = "needs VELOREN_ASSETS pointing at the Cromatolis assets"]
    fn real_sim_table_has_no_nan_river_cross_sections() {
        let p = load(0, None).expect("world generation");
        let size = p.world.sim().get_size().map(|e| e as i32);
        let csv = sim_table(&p, Vec2::zero(), size - Vec2::one());
        let rivers = assert_no_nan_cross_sections(&csv);
        assert!(rivers > 0, "no river chunks at all: wrong asset root?");
    }

    /// Real structures made of natural block kinds must not raise the ground:
    /// dump 400 x 400 m around the authored settlement "Falsepost" (a
    /// non-Kalthis/Duren/Portland site, read-only) and check that the surface
    /// the dump reports is the terrain. Before the sampler-based
    /// classification, 4 425 of those 160 000 columns reported a `ground_top`
    /// up to 22 m above the terrain and 2 598 were flagged `VOID` (building
    /// interiors).
    ///
    /// `VELOREN_ASSETS=/path/to/assets cargo test -p xindeler-terrain-probe
    /// -- --ignored real_site`
    #[test]
    #[ignore = "needs VELOREN_ASSETS pointing at the Cromatolis assets (Falsepost)"]
    fn real_site_structures_do_not_raise_the_ground() {
        let p = load(0, None).expect("world generation");
        let size = world_size(p.world.sim());
        let world = Box2 {
            x0: 0,
            y0: 0,
            x1: size.x,
            y1: size.y,
        };
        let ir = p.index.as_index_ref();
        let site = world_sites(&ir, world, 0)
            .into_iter()
            .find(|s| s.name.as_deref() == Some("Falsepost"))
            .expect("Falsepost is not in this asset root");
        let bx = Box2 {
            x0: site.wx - 200,
            y0: site.wy - 200,
            x1: site.wx + 200,
            y1: site.wy + 200,
        };
        let opts = DumpOpts {
            bx,
            zmin: None,
            zmax: None,
            site_margin: 0,
            keep_sprites: false,
            force: false,
        };
        let (d, _) = dump(&p, &opts, &|_, _| {}).unwrap();
        let ice = d.header.block_kinds["Ice"];
        let mut raised = 0;
        for i in 0..d.alt.len() {
            if d.alt[i].is_finite()
                && d.top_kind[i] != ice
                && i32::from(d.ground_top[i]) > d.alt[i] as i32
            {
                raised += 1;
            }
        }
        assert_eq!(
            raised, 0,
            "columns whose ground_top is above the terrain surface"
        );
        let above = d.header.stats["structure_above_ground_columns"];
        assert!(
            above > 100,
            "only {above} columns have structure above ground"
        );
        let voids = d.flags.iter().filter(|&&f| f & flag::VOID != 0).count();
        assert!(
            voids * 100 < d.alt.len(),
            "{voids} void columns: building interiors are being read as caves"
        );
    }

    /// Needs a real asset root: `VELOREN_ASSETS=/path/to/assets cargo test -p
    /// xindeler-terrain-probe --release -- --ignored`.
    #[test]
    #[ignore = "needs VELOREN_ASSETS pointing at the Cromatolis assets"]
    fn real_world_dump_is_deterministic_and_consistent() {
        let p = load(0, None).expect("world generation");
        assert!(
            p.assets_root.join("world/map/cromatolis_v0.bin").exists(),
            "{} is not a Cromatolis asset root",
            p.assets_root.display()
        );
        let opts = DumpOpts {
            bx: Box2::parse("22752,24576,22880,24704").unwrap(),
            zmin: None,
            zmax: None,
            site_margin: 600,
            keep_sprites: false,
            force: false,
        };
        let (a, stats) = dump(&p, &opts, &|_, _| {}).unwrap();
        assert_eq!(stats.chunks, 16);
        assert_eq!(stats.columns, 128 * 128);
        let mut bytes_a = Vec::new();
        a.write_to(&mut bytes_a).unwrap();
        let (b, _) = dump(&p, &opts, &|_, _| {}).unwrap();
        let mut bytes_b = Vec::new();
        b.write_to(&mut bytes_b).unwrap();
        assert!(bytes_a == bytes_b, "two dumps of the same box differ");
        let r = Dump::read_from(&bytes_a[..]).unwrap();
        assert_eq!(r.run_counts, a.run_counts);
        // Block fill rule: a dry column is solid up to `trunc(alt)`.
        let (mut dry, mut agree) = (0usize, 0usize);
        for i in 0..a.alt.len() {
            if a.flags[i] & format::flag::LIQUID == 0 {
                dry += 1;
                agree += usize::from(i32::from(a.ground_top[i]) == a.alt[i].trunc() as i32);
            }
        }
        assert!(
            dry > 0 && agree * 100 >= dry * 98,
            "{agree}/{dry} dry columns match trunc(alt)"
        );
        assert_eq!(a.header.stats["clipped_top_columns"], 0);
        assert_no_nan_cross_sections(&a.sim_csv);
    }

    // ------------------------------------------- natural vs structure blocks

    use common::{terrain::TerrainChunkMeta, vol::WriteVol};
    use vek::{Rgb, Vec3};

    fn blk(k: BlockKind) -> Block { Block::new(k, Rgb::new(90, 80, 70)) }

    /// Synthetic chunk (rock below z=0, air above) with column `(x, 0)` built
    /// by `fill(&mut set)`.
    fn synth(cols: &[(i32, &[(i32, i32, BlockKind)])]) -> TerrainChunk {
        let mut ch = TerrainChunk::new(
            0,
            blk(BlockKind::Rock),
            Block::air(SpriteKind::Empty),
            TerrainChunkMeta::void(),
        );
        for &(x, spans) in cols {
            for &(z0, z1, k) in spans {
                for z in z0..=z1 {
                    ch.set(Vec3::new(x, 0, z), blk(k)).unwrap();
                }
            }
        }
        ch
    }

    fn runs_of(ch: &TerrainChunk, x: i32, alt: f32) -> (Vec<(u8, u16)>, format::ColSummary) {
        let runs = column_runs(ch, Vec2::new(x, 0), -5, 30, false, surface_cap(alt));
        let sum = format::summarize(-5, &runs);
        (runs, sum)
    }

    /// Stone walls, keeps and roofs made of plain Rock above the sampler's
    /// surface are structure, not ground: they must not raise `ground_top`,
    /// create a void or change `top_kind`.
    #[test]
    fn natural_kind_blocks_above_the_surface_are_structure() {
        use BlockKind::*;
        let ch = synth(&[
            // Terrain to z=10 (grass on top), then a 5 m Rock wall.
            (0, &[(0, 9, Earth), (10, 10, Grass), (11, 15, Rock)]),
            // Terrain, a hollow stone keep: wall, gap, roof.
            (1, &[
                (0, 9, Earth),
                (10, 10, Grass),
                (11, 12, Rock),
                (14, 14, Rock),
            ]),
            // Frozen water above the surface is natural.
            (2, &[(0, 9, Earth), (10, 10, Grass), (11, 11, Ice)]),
            // Wood is structure whatever the altitude.
            (4, &[(0, 9, Earth), (10, 10, Grass), (11, 14, Wood)]),
            // A cave inside the terrain stays a void.
            (5, &[(0, 3, Rock), (5, 9, Earth), (10, 10, Grass)]),
        ]);
        let alt = 10.4;

        let (runs, s) = runs_of(&ch, 0, alt);
        assert_eq!(runs, vec![
            (class::GROUND, 16),
            (class::STRUCTURE, 5),
            (class::AIR, 14)
        ]);
        assert_eq!(s.ground_top, 10, "the wall must not raise ground_top");
        assert_eq!(
            ch.get(Vec3::new(0, 0, i32::from(s.ground_top)))
                .unwrap()
                .kind(),
            Grass,
            "top_kind is the natural surface block"
        );
        assert_ne!(s.flags & flag::STRUCTURE_ABOVE_GROUND, 0);
        assert_ne!(s.flags & flag::STRUCTURE, 0);
        assert_eq!(s.flags & flag::VOID, 0);

        // The keep: structure / air gap / structure, no void.
        let (runs, s) = runs_of(&ch, 1, alt);
        assert_eq!(runs, vec![
            (class::GROUND, 16),
            (class::STRUCTURE, 2),
            (class::AIR, 1),
            (class::STRUCTURE, 1),
            (class::AIR, 15)
        ]);
        assert_eq!(s.ground_top, 10);
        assert_eq!(s.flags & flag::VOID, 0, "a keep's interior is not a cave");
        assert_ne!(s.flags & flag::STRUCTURE_ABOVE_GROUND, 0);

        // Ice stays ground.
        let (_, s) = runs_of(&ch, 2, alt);
        assert_eq!(s.ground_top, 11);
        assert_eq!(s.flags & flag::STRUCTURE_ABOVE_GROUND, 0);

        // Without a sampler altitude the kind alone decides (the old rule).
        let (_, s) = runs_of(&ch, 0, f32::NAN);
        assert_eq!(s.ground_top, 15);
        assert_eq!(s.flags & flag::STRUCTURE_ABOVE_GROUND, 0);

        // Wood.
        let (_, s) = runs_of(&ch, 4, alt);
        assert_eq!(s.ground_top, 10);
        assert_ne!(s.flags & flag::STRUCTURE_ABOVE_GROUND, 0);

        // A real cave below the surface is still a void, with no structure.
        let (_, s) = runs_of(&ch, 5, alt);
        assert_eq!(s.ground_top, 10);
        assert_ne!(s.flags & flag::VOID, 0);
        assert_eq!(s.flags & flag::STRUCTURE_ABOVE_GROUND, 0);

        // A plain column has none of it.
        let (_, s) = runs_of(&ch, 7, 0.5);
        assert_eq!(
            s.flags & (flag::STRUCTURE | flag::STRUCTURE_ABOVE_GROUND),
            0
        );
    }

    /// The constant stretches below/above the chunk's stored range must be
    /// split where the surface falls inside them.
    #[test]
    fn surface_inside_a_constant_stretch_splits_the_run() {
        let ch = TerrainChunk::new(
            0,
            blk(BlockKind::Rock),
            blk(BlockKind::Rock),
            TerrainChunkMeta::void(),
        );
        let runs = column_runs(&ch, Vec2::new(3, 3), -5, 100, false, Some(40));
        assert_eq!(runs, vec![(class::GROUND, 46), (class::STRUCTURE, 59)]);
        // Cap above the whole range, below it, and absent.
        assert_eq!(
            column_runs(&ch, Vec2::new(3, 3), -5, 100, false, Some(500)),
            vec![(class::GROUND, 105)]
        );
        assert_eq!(
            column_runs(&ch, Vec2::new(3, 3), -5, 100, false, Some(-9)),
            vec![(class::STRUCTURE, 105)]
        );
        assert_eq!(
            column_runs(&ch, Vec2::new(3, 3), -5, 100, false, None),
            vec![(class::GROUND, 105)]
        );
    }

    #[test]
    fn z_range_is_validated_against_the_i16_format() {
        let auto = (-20, 300);
        assert_eq!(resolve_z_range(None, None, auto).unwrap(), (-20, 300));
        assert_eq!(resolve_z_range(Some(5), Some(9), auto).unwrap(), (5, 9));
        for (lo, hi) in [
            (Some(i32::MIN), Some(0)),
            (Some(0), Some(i32::MAX)),
            (Some(-32768), Some(0)),
            (Some(0), Some(32769)),
            (Some(9), Some(9)),
            (Some(10), Some(2)),
            (Some(-32767), Some(32768)),
            (Some(0), Some(40_000)),
        ] {
            assert!(resolve_z_range(lo, hi, auto).is_err(), "{lo:?}..{hi:?}");
        }
        // Edges that are legal.
        assert!(resolve_z_range(Some(-32767), Some(0), auto).is_ok());
        assert!(resolve_z_range(Some(1), Some(32768), auto).is_ok());
        // The automatic range is clamped into the legal window.
        assert!(resolve_z_range(None, None, (-99_999, 99_999)).is_err());
        assert!(
            resolve_z_range(None, None, (-40_000, -39_000)).is_err(),
            "an entirely out-of-range auto window is an error, not a wrap"
        );
        // Extreme box values do not overflow.
        assert!(Box2::parse("-2147483648,0,2147483647,5").is_err());
    }

    #[test]
    fn memory_preflight_refuses_huge_boxes_unless_forced() {
        assert!(check_memory(25_000_000, FAST_BYTES_PER_COLUMN, false).is_ok());
        let whole_world = 32768u64 * 24576;
        let e = check_memory(whole_world, FAST_BYTES_PER_COLUMN, false).unwrap_err();
        assert!(e.to_string().contains("--force"), "{e}");
        assert!(check_memory(whole_world, FAST_BYTES_PER_COLUMN, true).is_ok());
    }

    #[test]
    fn runs_builder_merges_adjacent_classes() {
        let mut r = Runs(Vec::new());
        r.push(1, 3);
        r.push(1, 2);
        r.push(0, 1);
        assert_eq!(r.0, vec![(1, 5), (0, 1)]);
        // The tallest legal column still fits one u16 run.
        let mut r = Runs(Vec::new());
        r.push(1, format::MAX_HEIGHT);
        assert_eq!(r.0, vec![(1, 32767)]);
    }
}

#[cfg(test)]
#[path = "probe_edge_tests.rs"]
mod edge_tests;
