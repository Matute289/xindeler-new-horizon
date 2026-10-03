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
    if b.is_liquid() {
        class::LIQUID
    } else if matches!(b.get_sprite(), Some(s) if s != SpriteKind::Empty) {
        if keep_sprites {
            class::SPRITE
        } else {
            class::AIR
        }
    } else if b.is_solid() {
        if matches!(
            b.kind(),
            BlockKind::Rock
                | BlockKind::WeakRock
                | BlockKind::GlowingRock
                | BlockKind::Grass
                | BlockKind::Snow
                | BlockKind::Earth
                | BlockKind::Sand
                | BlockKind::Ice
        ) {
            class::GROUND
        } else {
            class::STRUCTURE
        }
    } else {
        class::AIR
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

/// Default sampled z range for a box: from just under the lowest chunk
/// basement/altitude to 96 m above the highest altitude/water level.
pub fn auto_z_range(sim: &WorldSim, b: Box2) -> (i32, i32) {
    let (c0, c1) = b.chunk_range();
    let (mut lo, mut hi) = (f32::MAX, f32::MIN);
    for cy in c0.y..=c1.y {
        for cx in c0.x..=c1.x {
            if let Some(c) = sim.get(Vec2::new(cx, cy)) {
                lo = lo.min(c.alt.min(c.basement));
                hi = hi.max(c.alt.max(c.water_alt));
            }
        }
    }
    ((lo.floor() as i32 - 16).max(-4096), hi.ceil() as i32 + 96)
}

/// Bottom-to-top run builder.
struct Runs(Vec<(u8, u16)>);

impl Runs {
    fn push(&mut self, c: u8, n: i32) {
        let mut n = n;
        while n > 0 {
            let take = n.min(i32::from(u16::MAX)) as u16;
            match self.0.last_mut() {
                Some((lc, ln))
                    if *lc == c && u32::from(*ln) + u32::from(take) <= u32::from(u16::MAX) =>
                {
                    *ln += take
                },
                _ => self.0.push((c, take)),
            }
            n -= i32::from(take);
        }
    }
}

/// Classes of the column at chunk-local `rel` for `z in [zmin, zmax)`, as runs.
/// Blocks below/above the chunk's stored range are constant, so they are
/// emitted as one run each instead of being queried one by one.
pub(crate) fn column_runs(
    ch: &TerrainChunk,
    rel: Vec2<i32>,
    zmin: i32,
    zmax: i32,
    keep_sprites: bool,
) -> Vec<(u8, u16)> {
    let class_at = |z: i32| {
        ch.get(rel.with_z(z))
            .map_or(class::UNLOADED, |b| classify(b, keep_sprites))
    };
    let (cmin, cmax) = (ch.get_min_z(), ch.get_max_z());
    let mut r = Runs(Vec::with_capacity(8));
    let lo_end = cmin.clamp(zmin, zmax);
    if lo_end > zmin {
        r.push(class_at(zmin), lo_end - zmin);
    }
    let hi_start = cmax.clamp(zmin, zmax).max(lo_end);
    for z in lo_end..hi_start {
        r.push(class_at(z), 1);
    }
    if zmax > hi_start {
        r.push(class_at(hi_start), zmax - hi_start);
    }
    r.0
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

fn chunk_columns(
    p: &Probe,
    b: Box2,
    key: Vec2<i32>,
    zrange: (i32, i32),
    keep_sprites: bool,
) -> Res<Vec<ColOut>> {
    let (zmin, zmax) = zrange;
    let ir = p.index.as_index_ref();
    let ch = p
        .world
        .generate_chunk(ir, key, None, || false, None, None)
        .map_err(|()| format!("generate_chunk failed at {key}"))?
        .0;
    let cg = p.world.sample_columns();
    let base = key * CHUNK;
    let (lx0, lx1) = (
        (b.x0 - base.x).clamp(0, CHUNK),
        (b.x1 - base.x).clamp(0, CHUNK),
    );
    let (ly0, ly1) = (
        (b.y0 - base.y).clamp(0, CHUNK),
        (b.y1 - base.y).clamp(0, CHUNK),
    );
    // Row-major (y, then x) over the in-box part of the chunk.
    let mut out = Vec::with_capacity(((lx1 - lx0) * (ly1 - ly0)) as usize);
    for ly in ly0..ly1 {
        for lx in lx0..lx1 {
            let rel = Vec2::new(lx, ly);
            let w = base + rel;
            let runs = column_runs(&ch, rel, zmin, zmax, keep_sprites);
            let sum = format::summarize(zmin, &runs);
            let top_kind = if sum.ground_top == format::NO_Z {
                255
            } else {
                ch.get(rel.with_z(i32::from(sum.ground_top)))
                    .map_or(255, |bl| bl.kind() as u8)
            };
            let (alt, riverless_alt, water_level, warp_factor) = cg
                .get((w, ir, None))
                .map_or((f32::NAN, f32::NAN, f32::NAN, f32::NAN), |s| {
                    (s.alt, s.riverless_alt, s.water_level, s.warp_factor)
                });
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
    Ok(out)
}

pub struct DumpOpts {
    pub bx: Box2,
    pub zmin: Option<i32>,
    pub zmax: Option<i32>,
    pub site_margin: i32,
    /// Record sprite blocks as `SPRITE` instead of air (not reproducible).
    pub keep_sprites: bool,
}

pub struct DumpStats {
    pub chunks: usize,
    pub columns: usize,
    pub gen_secs: f32,
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
    let (auto_lo, auto_hi) = auto_z_range(p.world.sim(), b);
    let (zmin, zmax) = (o.zmin.unwrap_or(auto_lo), o.zmax.unwrap_or(auto_hi));
    if zmax <= zmin || zmax - zmin > i32::from(i16::MAX) {
        return Err(format!("bad z range {zmin}..{zmax} (height must be 1..=32767)").into());
    }
    let (c0, c1) = b.chunk_range();
    let ncx = (c1.x - c0.x + 1) as usize;
    let ncy = (c1.y - c0.y + 1) as usize;
    let n = b.nx() * b.ny();

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
        let results: Vec<Res<Vec<ColOut>>> = p.pool.install(|| {
            keys.par_iter()
                .map(|&k| chunk_columns(p, b, k, (zmin, zmax), o.keep_sprites))
                .collect()
        });
        let mut cols: Vec<Vec<ColOut>> = Vec::with_capacity(results.len());
        for r in results {
            cols.push(r?);
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

fn near_box(x: i32, y: i32, b: Box2, margin: i32) -> bool {
    x > b.x0 - margin && x < b.x1 + margin && y > b.y0 - margin && y < b.y1 + margin
}

/// Sites in the generated world index whose origin lies within `margin` of the
/// box.
pub fn world_sites(ir: &world::IndexRef, b: Box2, margin: i32) -> Vec<SiteRec> {
    let mut out = Vec::new();
    for (_, site) in ir.sites.iter() {
        let o = site.origin;
        if near_box(o.x, o.y, b, margin) {
            out.push(SiteRec {
                source: "world_site".into(),
                id: None,
                name: site.name().map(str::to_owned),
                kind: site.kind.as_ref().map(|k| format!("{k:?}")),
                wx: o.x,
                wy: o.y,
                radius: Some(site.radius()),
            });
        }
    }
    out.sort_by(|a, b| (a.wx, a.wy, &a.name).cmp(&(b.wx, b.wy, &b.name)));
    out
}

/// All authored points (settlements, landmarks, caves, bridges, interiors...)
/// from the `cromatolis_v0_*.ron` files with normalised top-left coordinates,
/// converted to world metres, that lie within `margin` of the box.
pub fn authored_points(root: &Path, size: Vec2<i32>, b: Box2, margin: i32) -> Vec<SiteRec> {
    let mut all = Vec::new();
    all_authored_points(root, size, &mut all);
    all.retain(|r| near_box(r.wx, r.wy, b, margin));
    all.sort_by(|a, b| (&a.source, a.wx, a.wy, &a.id).cmp(&(&b.source, b.wx, b.wy, &b.id)));
    all
}

/// Every normalised authored point, unfiltered.
pub fn all_authored_points(root: &Path, size: Vec2<i32>, out: &mut Vec<SiteRec>) {
    let Ok(rd) = std::fs::read_dir(root.join("world/map")) else {
        return;
    };
    let mut files: Vec<_> = rd
        .flatten()
        .filter(|e| {
            let n = e.file_name().to_string_lossy().into_owned();
            n.starts_with("cromatolis_v0_") && n.ends_with(".ron")
        })
        .collect();
    files.sort_by_key(|e| e.file_name());
    for e in files {
        let name = e.file_name().to_string_lossy().into_owned();
        let Ok(text) = std::fs::read_to_string(e.path()) else {
            continue;
        };
        let Ok(v) = ron::from_str::<ron::Value>(&text) else {
            continue;
        };
        if !coordinate_space_is_normalized(&v) {
            continue;
        }
        collect_points(&v, &name, size, None, None, None, out);
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

fn collect_points(
    v: &ron::Value,
    file: &str,
    size: Vec2<i32>,
    id: Option<&str>,
    name: Option<&str>,
    kind: Option<&str>,
    out: &mut Vec<SiteRec>,
) {
    match v {
        ron::Value::Map(m) => {
            let id_s = field(m, "id").and_then(str_of);
            let name_s = field(m, "name").and_then(str_of);
            let kind_s = field(m, "kind")
                .or_else(|| field(m, "category"))
                .and_then(|k| match k {
                    ron::Value::String(s) => Some(s.clone()),
                    _ => None,
                });
            let (id, name, kind) = (
                id_s.as_deref().or(id),
                name_s.as_deref().or(name),
                kind_s.as_deref().or(kind),
            );
            if let (Some(x), Some(y)) = (field(m, "x").and_then(num), field(m, "y").and_then(num))
                && (0.0..=1.0).contains(&x)
                && (0.0..=1.0).contains(&y)
            {
                out.push(SiteRec {
                    source: file.to_string(),
                    id: id.map(str::to_owned),
                    name: name.map(str::to_owned),
                    kind: kind.map(str::to_owned),
                    wx: (x * f64::from(size.x)).round() as i32,
                    wy: ((1.0 - y) * f64::from(size.y)).round() as i32,
                    radius: None,
                });
                return;
            }
            for (_, child) in m.iter() {
                collect_points(child, file, size, id, name, kind, out);
            }
        },
        ron::Value::Seq(items) => {
            for item in items {
                collect_points(item, file, size, id, name, kind, out);
            }
        },
        ron::Value::Option(Some(inner)) => collect_points(inner, file, size, id, name, kind, out),
        _ => {},
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
    fn box_parse_and_chunks() {
        let b = Box2::parse("0,0,33,64").unwrap();
        assert_eq!((b.nx(), b.ny()), (33, 64));
        let (c0, c1) = b.chunk_range();
        assert_eq!((c0, c1), (Vec2::new(0, 0), Vec2::new(1, 1)));
        assert!(Box2::parse("5,5,5,9").is_err());
        assert!(Box2::parse("1,2,3").is_err());
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
    }

    #[test]
    fn runs_builder_merges_and_splits() {
        let mut r = Runs(Vec::new());
        r.push(1, 3);
        r.push(1, 2);
        r.push(0, 1);
        assert_eq!(r.0, vec![(1, 5), (0, 1)]);
        let mut r = Runs(Vec::new());
        r.push(1, 70_000);
        assert_eq!(r.0.iter().map(|&(_, n)| u32::from(n)).sum::<u32>(), 70_000);
    }
}
