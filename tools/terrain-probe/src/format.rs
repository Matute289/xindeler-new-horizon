//! The `tprobe v1` dump format: reader, writer and the pure helpers around
//! them. Nothing here touches the engine, so the round-trip tests run without
//! assets.
//!
//! Layout of a file:
//!
//! ```text
//! "TPROBE1\n"                      8 bytes magic
//! u32 LE  header_len
//! header_len bytes of JSON         `Header` (includes the section table)
//! sections, in `Header::sections` order, each `comp_len` bytes of zstd
//! ```
//!
//! Columns are row-major over the box: `idx = (y - y0) * nx + (x - x0)`, so
//! `y` grows north and a numpy reshape `(ny, nx)` gives the map orientation
//! with row 0 = southernmost. Multi-byte arrays are stored little-endian and
//! byte-shuffled (all byte-0s, then all byte-1s, ...) before zstd, which
//! typically shrinks them 2-3x versus plain LE.
//!
//! The file contains no timestamps or host info: the same world + box + seed
//! always produces identical bytes.

use std::{
    borrow::Cow,
    collections::BTreeMap,
    io::{self, Read, Write},
    sync::{
        Mutex,
        atomic::{AtomicUsize, Ordering},
    },
};

use serde::{Deserialize, Serialize};

pub const MAGIC: &[u8; 8] = b"TPROBE1\n";
pub const FORMAT_NAME: &str = "tprobe v1";
pub const ZSTD_LEVEL: i32 = 3;

/// Block classes (one byte per block, run-length encoded per column).
pub mod class {
    /// Air (non-solid, non-liquid, no sprite).
    pub const AIR: u8 = 0;
    /// Natural terrain: a Rock, WeakRock, GlowingRock, Grass, Snow, Earth, Sand
    /// or Ice block that is not above the column sampler's surface
    /// (`z <= trunc(alt)`; Ice is exempt, frozen water sits above `alt`).
    /// Natural-kind blocks above the surface are class 4.
    pub const GROUND: u8 = 1;
    /// Liquid (water, lava).
    pub const LIQUID: u8 = 2;
    /// Not loaded / not generated (client path only).
    pub const UNLOADED: u8 = 3;
    /// Any solid, non-sprite block that is not natural terrain: structures,
    /// wood, leaves, and Rock/Earth/Sand/... blocks standing above the
    /// sampler's surface (stone walls and keeps, bridge decks, boulders,
    /// debris, authored floating islands).
    pub const STRUCTURE: u8 = 4;
    /// A sprite block, solid or not (flowers, tufts, torches, chests...).
    /// Sprite placement uses the engine's dynamic RNG, so it is the one
    /// class that is not reproducible between runs.
    pub const SPRITE: u8 = 5;
}

/// Per-column flag bits (`Dump::flags`).
pub mod flag {
    pub const STRUCTURE: u8 = 1;
    pub const SPRITE: u8 = 2;
    /// A non-solid run (air, liquid or sprite) lies below the topmost natural
    /// ground block: a cave, shaft, authored void or underground gap.
    pub const VOID: u8 = 4;
    pub const LIQUID: u8 = 8;
    /// The topmost block of the sampled z range is not air: `zmax` clipped the
    /// column, raise `--zmax`.
    pub const CLIPPED_TOP: u8 = 16;
    /// A [`class::STRUCTURE`] block lies above `ground_top` (a wall, keep,
    /// tree, bridge deck, boulder... standing on the surface), or the
    /// column has structure blocks and no natural ground at all.
    pub const STRUCTURE_ABOVE_GROUND: u8 = 32;
}

pub const NO_Z: i16 = i16::MIN;

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct SectionInfo {
    pub name: String,
    pub raw_len: u64,
    pub comp_len: u64,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct Header {
    /// Always [`FORMAT_NAME`].
    pub format: String,
    /// `[x0, y0, x1, y1]` in world metres, half-open (`x0 <= x < x1`).
    pub box_xy: [i32; 4],
    /// Sampled z range, half-open.
    pub zmin: i32,
    pub zmax: i32,
    pub nx: u32,
    pub ny: u32,
    pub seed: u32,
    /// `"fast"` (generate_chunk) or `"client"` (blocks the client received).
    pub path: String,
    /// Calendar used for sprite generation; `null` = none (the research
    /// default).
    pub calendar: Option<String>,
    pub engine_commit: String,
    /// sha256 of every `cromatolis_v0*` asset under `world/map/`.
    pub assets: BTreeMap<String, String>,
    /// Name -> code of the block classes, so a reader need not hardcode them.
    pub class_codes: BTreeMap<String, u8>,
    /// `BlockKind` name -> code, for decoding `top_kind`.
    #[serde(default)]
    pub block_kinds: BTreeMap<String, u8>,
    /// Free-form counters (columns, chunks, clipped columns, ...).
    pub stats: BTreeMap<String, u64>,
    /// Only on `path = "client"` dumps: how the blocks were streamed. Absent
    /// (and not serialised) on fast dumps, so their bytes are unchanged.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub client: Option<ClientInfo>,
    pub sections: Vec<SectionInfo>,
}

/// One tile of a client dump: a rectangle of chunks the bot streamed from one
/// teleport position.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct TileInfo {
    /// Inclusive chunk range `[cx0, cy0, cx1, cy1]`.
    pub chunks: [i32; 4],
    /// Teleport target `[x, y, z]` sent with `/goto`.
    pub goto: [i32; 3],
    /// Where the bot stood when the tile was harvested.
    pub landing: Option<[f32; 3]>,
    pub streamed: u64,
    pub total: u64,
    pub secs: f32,
}

/// Provenance of a client-path dump.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct ClientInfo {
    /// Terrain view distance the bot requested, in chunks.
    pub view_distance: u32,
    pub chunks_total: u64,
    pub chunks_streamed: u64,
    /// Chunk keys `[cx, cy]` that never arrived (their columns are class 3).
    pub missing_chunks: Vec<[i32; 2]>,
    pub tiles: Vec<TileInfo>,
}

impl ClientInfo {
    pub fn streamed_pct(&self) -> f64 {
        self.chunks_streamed as f64 * 100.0 / self.chunks_total.max(1) as f64
    }
}

impl Header {
    pub fn class_codes() -> BTreeMap<String, u8> {
        [
            ("air", class::AIR),
            ("ground", class::GROUND),
            ("liquid", class::LIQUID),
            ("unloaded", class::UNLOADED),
            ("structure", class::STRUCTURE),
            ("sprite", class::SPRITE),
        ]
        .into_iter()
        .map(|(k, v)| (k.to_string(), v))
        .collect()
    }

    pub fn columns(&self) -> usize { self.nx as usize * self.ny as usize }

    /// Blocks per column (0 for an inverted range; [`Self::check_dims`]
    /// rejects those, and no `i32` overflow is possible here).
    pub fn height(&self) -> usize {
        (i64::from(self.zmax) - i64::from(self.zmin)).clamp(0, i64::from(i32::MAX)) as usize
    }

    /// Reject headers no writer of this tool produces: an inconsistent box,
    /// too many columns, or a z range outside [`Z_MIN_ALLOWED`]..=
    /// [`Z_MAX_ALLOWED`] / taller than [`MAX_HEIGHT`].
    pub fn check_dims(&self) -> io::Result<()> {
        let [x0, y0, x1, y1] = self.box_xy;
        let (w, h) = (i64::from(x1) - i64::from(x0), i64::from(y1) - i64::from(y0));
        if w <= 0 || h <= 0 || w != i64::from(self.nx) || h != i64::from(self.ny) {
            return Err(bad_data(format!(
                "box {:?} does not match {}x{} columns",
                self.box_xy, self.nx, self.ny
            )));
        }
        if u64::from(self.nx) * u64::from(self.ny) > MAX_COLUMNS {
            return Err(bad_data(format!(
                "{}x{} columns exceeds the {MAX_COLUMNS} column limit",
                self.nx, self.ny
            )));
        }
        check_z_range(self.zmin, self.zmax).map_err(bad_data)
    }
}

/// Lowest `zmin` a dump can hold (`-32768` is the "no z" sentinel).
pub const Z_MIN_ALLOWED: i32 = -32767;
/// Highest `zmax` (half-open) a dump can hold: the top block is `zmax - 1`,
/// which must fit an `i16`.
pub const Z_MAX_ALLOWED: i32 = 32768;
/// Most blocks per column.
pub const MAX_HEIGHT: i32 = 32767;

/// Validate a half-open sampled z range against what a dump can represent.
pub fn check_z_range(zmin: i32, zmax: i32) -> Result<(), String> {
    if zmin < Z_MIN_ALLOWED || zmax > Z_MAX_ALLOWED {
        return Err(format!(
            "z range {zmin}..{zmax} outside {Z_MIN_ALLOWED}..={Z_MAX_ALLOWED}: z values are \
             stored as i16 and -32768 is the \"none\" sentinel"
        ));
    }
    if zmax <= zmin || zmax - zmin > MAX_HEIGHT {
        return Err(format!(
            "bad z range {zmin}..{zmax} (height must be 1..={MAX_HEIGHT})"
        ));
    }
    Ok(())
}

/// A site / landmark / authored point near the box.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct SiteRec {
    /// `"world_site"` for sites in the generated world index, otherwise the
    /// `cromatolis_v0_*.ron` file the point came from.
    pub source: String,
    pub id: Option<String>,
    pub name: Option<String>,
    pub kind: Option<String>,
    pub wx: i32,
    pub wy: i32,
    /// Site radius in metres when known.
    pub radius: Option<f32>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Dump {
    pub header: Header,
    // Per column (len = nx * ny).
    pub alt: Vec<f32>,
    pub riverless_alt: Vec<f32>,
    pub water_level: Vec<f32>,
    pub warp_factor: Vec<f32>,
    /// `BlockKind as u8` of the topmost natural ground block (255 = none).
    pub top_kind: Vec<u8>,
    pub flags: Vec<u8>,
    /// z of the topmost natural ground block ([`NO_Z`] = none).
    pub ground_top: Vec<i16>,
    /// z of the topmost liquid block, so the water surface is `water_top + 1`
    /// ([`NO_Z`] = none).
    pub water_top: Vec<i16>,
    /// Liquid blocks above `ground_top`.
    pub liquid_depth: Vec<u16>,
    // Run-length encoded block classes, bottom (zmin) to top (zmax).
    /// Number of runs per column.
    pub run_counts: Vec<u16>,
    /// Concatenated run classes (all columns).
    pub run_class: Vec<u8>,
    /// Concatenated run lengths (all columns).
    pub run_len: Vec<u16>,
    /// Per-chunk sim table, CSV with a header row.
    pub sim_csv: String,
    pub sites: Vec<SiteRec>,
}

/// Summary of one column, computed from its runs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ColSummary {
    pub ground_top: i16,
    pub water_top: i16,
    pub liquid_depth: u16,
    pub flags: u8,
}

/// Run-length encode a bottom-to-top iterator of block classes.
#[cfg(test)]
pub fn rle(classes: impl IntoIterator<Item = u8>) -> Vec<(u8, u16)> {
    let mut out: Vec<(u8, u16)> = Vec::new();
    for c in classes {
        match out.last_mut() {
            Some((lc, n)) if *lc == c && *n < u16::MAX => *n += 1,
            _ => out.push((c, 1)),
        }
    }
    out
}

/// Derive ground/water tops, liquid depth and flags from a column's runs.
///
/// "Ground" means [`class::GROUND`]: natural-kind blocks that the classifier
/// accepted as terrain (see `probe::classify_at`). Natural-kind blocks that
/// stand above the sampler's surface arrive here already as
/// [`class::STRUCTURE`], so they neither raise `ground_top` nor create
/// [`flag::VOID`]; they raise [`flag::STRUCTURE_ABOVE_GROUND`] instead.
pub fn summarize(zmin: i32, runs: &[(u8, u16)]) -> ColSummary {
    // i64 throughout: `zmin` may come from a corrupt header.
    let mut z = i64::from(zmin);
    let mut ground_top = i64::MIN;
    let mut water_top = i64::MIN;
    let mut struct_top = i64::MIN;
    let mut flags = 0u8;
    // Pass 1: tops and the plain flags.
    for &(c, n) in runs {
        let top = z + i64::from(n) - 1;
        match c {
            class::GROUND => ground_top = top,
            class::LIQUID => {
                water_top = top;
                flags |= flag::LIQUID;
            },
            class::STRUCTURE => {
                struct_top = top;
                flags |= flag::STRUCTURE;
            },
            class::SPRITE => flags |= flag::SPRITE,
            _ => {},
        }
        z += i64::from(n);
    }
    if let Some(&(c, _)) = runs.last()
        && c != class::AIR
        && c != class::SPRITE
        && c != class::UNLOADED
    {
        flags |= flag::CLIPPED_TOP;
    }
    if struct_top != i64::MIN && struct_top > ground_top {
        flags |= flag::STRUCTURE_ABOVE_GROUND;
    }
    // Pass 2: voids and liquid depth, relative to the ground top.
    let mut liquid_depth = 0u64;
    let mut z = i64::from(zmin);
    for &(c, n) in runs {
        let top = z + i64::from(n) - 1;
        let non_solid = matches!(c, class::AIR | class::LIQUID | class::SPRITE);
        if ground_top != i64::MIN {
            if non_solid && top < ground_top {
                flags |= flag::VOID;
            }
            if c == class::LIQUID && z > ground_top {
                liquid_depth += u64::from(n);
            }
        }
        z += i64::from(n);
    }
    let as_z = |v: i64| i16::try_from(v).ok().filter(|&t| t != NO_Z).unwrap_or(NO_Z);
    ColSummary {
        ground_top: as_z(ground_top),
        water_top: as_z(water_top),
        liquid_depth: liquid_depth.min(u64::from(u16::MAX)) as u16,
        flags,
    }
}

impl Dump {
    /// Offset of each column's first run in `run_class` / `run_len`
    /// (`len = columns + 1`).
    pub fn run_offsets(&self) -> Vec<usize> {
        let mut off = Vec::with_capacity(self.run_counts.len() + 1);
        let mut acc = 0usize;
        off.push(0);
        for &c in &self.run_counts {
            acc += usize::from(c);
            off.push(acc);
        }
        off
    }

    pub fn col_index(&self, x: i32, y: i32) -> Option<usize> {
        let [x0, y0, x1, y1] = self.header.box_xy;
        (x >= x0 && x < x1 && y >= y0 && y < y1)
            .then(|| (y - y0) as usize * self.header.nx as usize + (x - x0) as usize)
    }

    /// Runs of column `idx` (`off` comes from [`Self::run_offsets`]).
    pub fn runs_at(&self, off: &[usize], idx: usize) -> impl Iterator<Item = (u8, u16)> + '_ {
        let (a, b) = (off[idx], off[idx + 1]);
        self.run_class[a..b]
            .iter()
            .copied()
            .zip(self.run_len[a..b].iter().copied())
    }

    /// Class of the block at `(col idx, z)`, or `None` outside `[zmin, zmax)`.
    #[allow(dead_code)] // reader API for verifier code, exercised by the tests
    pub fn class_at(&self, off: &[usize], idx: usize, z: i32) -> Option<u8> {
        if z < self.header.zmin || z >= self.header.zmax {
            return None;
        }
        let mut cur = self.header.zmin;
        for (c, n) in self.runs_at(off, idx) {
            cur += i32::from(n);
            if z < cur {
                return Some(c);
            }
        }
        None
    }

    /// Check internal consistency (lengths, run sums).
    pub fn validate(&self) -> io::Result<()> {
        self.header.check_dims()?;
        let n = self.header.columns();
        let bad = |m: String| Err(bad_data(m));
        for (name, len) in [
            ("alt", self.alt.len()),
            ("riverless_alt", self.riverless_alt.len()),
            ("water_level", self.water_level.len()),
            ("warp_factor", self.warp_factor.len()),
            ("top_kind", self.top_kind.len()),
            ("flags", self.flags.len()),
            ("ground_top", self.ground_top.len()),
            ("water_top", self.water_top.len()),
            ("liquid_depth", self.liquid_depth.len()),
            ("run_counts", self.run_counts.len()),
        ] {
            if len != n {
                return bad(format!("{name} has {len} entries, expected {n}"));
            }
        }
        let total: usize = self.run_counts.iter().map(|&c| usize::from(c)).sum();
        if self.run_class.len() != total || self.run_len.len() != total {
            return bad(format!(
                "run arrays have {}/{} entries, run_counts sum to {total}",
                self.run_class.len(),
                self.run_len.len()
            ));
        }
        let h = self.header.height() as u64;
        let off = self.run_offsets();
        for i in 0..n {
            let s: u64 = self.run_len[off[i]..off[i + 1]]
                .iter()
                .map(|&l| u64::from(l))
                .sum();
            if s != h {
                return bad(format!("column {i}: runs cover {s} blocks, expected {h}"));
            }
        }
        Ok(())
    }
}

// ---------------------------------------------------------------- encoding

/// Largest header JSON the reader accepts (real headers are a few KB; a
/// 5 km client dump with thousands of missing chunks is well under 1 MB).
pub const MAX_HEADER_LEN: u32 = 16 << 20;
/// Largest `sim_csv` / `sites_json` section the reader accepts (the whole
/// Cromatolis world's sim table is about 120 MB).
const MAX_TEXT_SECTION: u64 = 512 << 20;
/// Most columns a header may declare (2^31, i.e. 46 340 x 46 340 m).
pub const MAX_COLUMNS: u64 = 1 << 31;
/// zstd cannot expand a frame by more than about 32 768x (a 128 KiB RLE block
/// costs 4 bytes); a section declaring more than this is lying.
const MAX_RATIO: u64 = 40_000;
/// Worker threads compressing sections at once. Each holds one raw section
/// (up to ~6 bytes per run) plus its compressed output while it runs, so this
/// bounds the write-path memory: 2 workers cost +0.28 GB over the in-memory
/// dump on a 25 M column box (4 cost +0.49 GB and the old writer +0.91 GB).
const COMPRESS_WORKERS: usize = 2;

/// Lane-shuffle `v` straight into the output: all byte-0s, then all byte-1s,
/// ... One allocation, no intermediate little-endian copy.
fn pack_lanes<const N: usize, T: Copy>(v: &[T], to_le: fn(T) -> [u8; N]) -> Vec<u8> {
    let n = v.len();
    let mut out = vec![0u8; n * N];
    for (i, &x) in v.iter().enumerate() {
        for (k, b) in to_le(x).into_iter().enumerate() {
            out[k * n + i] = b;
        }
    }
    out
}

/// Inverse of [`pack_lanes`]. `raw.len()` must be a multiple of `N`.
fn unpack_lanes<const N: usize, T>(raw: &[u8], from_le: fn([u8; N]) -> T) -> io::Result<Vec<T>> {
    if !raw.len().is_multiple_of(N) {
        return Err(bad_data(format!(
            "section length {} is not a multiple of {N}",
            raw.len()
        )));
    }
    let n = raw.len() / N;
    Ok((0..n)
        .map(|i| {
            let mut a = [0u8; N];
            for (k, b) in a.iter_mut().enumerate() {
                *b = raw[k * n + i];
            }
            from_le(a)
        })
        .collect())
}

fn bad_data(m: impl Into<String>) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, m.into())
}

/// Section names, in file order.
const SECTION_NAMES: [&str; 14] = [
    "alt",
    "riverless_alt",
    "water_level",
    "warp_factor",
    "top_kind",
    "flags",
    "ground_top",
    "water_top",
    "liquid_depth",
    "run_counts",
    "run_class",
    "run_len",
    "sim_csv",
    "sites_json",
];

/// Read exactly `n` bytes without trusting `n` for the allocation: the buffer
/// grows only as bytes arrive, so a lying length on a short file costs the
/// file size, not `n`.
fn read_exact_vec<R: Read>(r: &mut R, n: u64) -> io::Result<Vec<u8>> {
    let mut v = Vec::new();
    r.by_ref().take(n).read_to_end(&mut v)?;
    if v.len() as u64 != n {
        return Err(io::Error::new(
            io::ErrorKind::UnexpectedEof,
            format!("file ends after {} of {n} declared bytes", v.len()),
        ));
    }
    Ok(v)
}

/// Read and decompress one section. `expect` is the exact raw length the
/// header implies (`None` for the text sections, capped at
/// [`MAX_TEXT_SECTION`]).
fn read_section<R: Read>(
    r: &mut R,
    dec: &mut zstd::bulk::Decompressor<'_>,
    info: &SectionInfo,
    expect: Option<u64>,
) -> io::Result<Vec<u8>> {
    let name = &info.name;
    match expect {
        Some(e) if e != info.raw_len => {
            return Err(bad_data(format!(
                "section {name}: declares {} raw bytes, the header implies {e}",
                info.raw_len
            )));
        },
        None if info.raw_len > MAX_TEXT_SECTION => {
            return Err(bad_data(format!(
                "section {name}: {} raw bytes exceeds the {MAX_TEXT_SECTION} limit",
                info.raw_len
            )));
        },
        _ => {},
    }
    if info.raw_len > info.comp_len.saturating_mul(MAX_RATIO).saturating_add(1024) {
        return Err(bad_data(format!(
            "section {name}: {} raw bytes from {} compressed is beyond zstd's maximum ratio",
            info.raw_len, info.comp_len
        )));
    }
    let comp = read_exact_vec(r, info.comp_len)?;
    let cap = usize::try_from(info.raw_len)
        .map_err(|_| bad_data(format!("section {name}: raw length does not fit in memory")))?;
    let mut out: Vec<u8> = Vec::new();
    out.try_reserve_exact(cap)
        .map_err(|e| bad_data(format!("section {name}: cannot reserve {cap} bytes: {e}")))?;
    let got = dec.decompress_to_buffer(&comp[..], &mut out)?;
    if got != cap || out.len() != cap {
        return Err(bad_data(format!("section {name}: wrong decompressed size")));
    }
    Ok(out)
}

impl Dump {
    /// Raw (shuffled, little-endian) bytes of section `i`. Byte sections are
    /// borrowed, typed ones are packed into a single fresh buffer.
    fn section_raw<'a>(&'a self, i: usize, sites_json: &'a [u8]) -> Cow<'a, [u8]> {
        let f32s = |v: &[f32]| Cow::Owned(pack_lanes(v, f32::to_le_bytes));
        let i16s = |v: &[i16]| Cow::Owned(pack_lanes(v, i16::to_le_bytes));
        let u16s = |v: &[u16]| Cow::Owned(pack_lanes(v, u16::to_le_bytes));
        match i {
            0 => f32s(&self.alt),
            1 => f32s(&self.riverless_alt),
            2 => f32s(&self.water_level),
            3 => f32s(&self.warp_factor),
            4 => Cow::Borrowed(&self.top_kind),
            5 => Cow::Borrowed(&self.flags),
            6 => i16s(&self.ground_top),
            7 => i16s(&self.water_top),
            8 => u16s(&self.liquid_depth),
            9 => u16s(&self.run_counts),
            10 => Cow::Borrowed(&self.run_class),
            11 => u16s(&self.run_len),
            12 => Cow::Borrowed(self.sim_csv.as_bytes()),
            _ => Cow::Borrowed(sites_json),
        }
    }

    /// Build, compress and drop every section, [`COMPRESS_WORKERS`] at a time.
    /// Each section is one zstd frame, so the bytes do not depend on the
    /// number of workers; results are placed by section index.
    fn compress_sections(&self, sites_json: &[u8]) -> io::Result<Vec<(u64, Vec<u8>)>> {
        // Biggest sections first so the workers finish together.
        let mut order: Vec<usize> = (0..SECTION_NAMES.len()).collect();
        let weight = |i: usize| match i {
            11 => self.run_len.len() * 2,
            10 => self.run_class.len(),
            0..=3 => self.alt.len() * 4,
            12 => self.sim_csv.len(),
            13 => sites_json.len(),
            _ => self.alt.len() * 2,
        };
        order.sort_by_key(|&i| std::cmp::Reverse(weight(i)));
        let next = AtomicUsize::new(0);
        let slots: Vec<Mutex<Option<io::Result<(u64, Vec<u8>)>>>> =
            order.iter().map(|_| Mutex::new(None)).collect();
        let workers = COMPRESS_WORKERS.min(order.len());
        std::thread::scope(|sc| {
            for _ in 0..workers {
                sc.spawn(|| {
                    loop {
                        let k = next.fetch_add(1, Ordering::Relaxed);
                        let Some(&sec) = order.get(k) else { break };
                        let res = (|| {
                            let raw = self.section_raw(sec, sites_json);
                            let mut c = zstd::bulk::Compressor::new(ZSTD_LEVEL)?.compress(&raw)?;
                            c.shrink_to_fit();
                            Ok((raw.len() as u64, c))
                        })();
                        *slots[sec].lock().unwrap() = Some(res);
                    }
                });
            }
        });
        // `slots` is indexed by section number, so placement is fixed.
        slots
            .into_iter()
            .map(|m| {
                m.into_inner()
                    .unwrap()
                    .unwrap_or_else(|| Err(io::Error::other("section was not compressed")))
            })
            .collect()
    }

    /// Serialise to `w`. `self.header.sections` is rebuilt; everything else in
    /// the header is written as given.
    pub fn write_to<W: Write>(&self, mut w: W) -> io::Result<()> {
        self.validate()?;
        let sites_json = serde_json::to_vec(&self.sites).map_err(io::Error::other)?;
        let comp = self.compress_sections(&sites_json)?;
        let mut header = self.header.clone();
        header.sections = SECTION_NAMES
            .iter()
            .zip(&comp)
            .map(|(name, (raw_len, c))| SectionInfo {
                name: (*name).to_string(),
                raw_len: *raw_len,
                comp_len: c.len() as u64,
            })
            .collect();
        let hjson = serde_json::to_vec(&header).map_err(io::Error::other)?;
        let hlen = u32::try_from(hjson.len())
            .ok()
            .filter(|&n| n <= MAX_HEADER_LEN)
            .ok_or_else(|| bad_data("header JSON too large"))?;
        w.write_all(MAGIC)?;
        w.write_all(&hlen.to_le_bytes())?;
        w.write_all(&hjson)?;
        for (_, c) in &comp {
            w.write_all(c)?;
        }
        w.flush()
    }

    /// Read a dump. Never aborts on malformed input: every length the file
    /// declares is checked against what the header implies and against the
    /// bytes that actually arrive before memory is reserved, so a corrupt or
    /// truncated file yields `Err`, not an allocation of the declared size.
    pub fn read_from<R: Read>(mut r: R) -> io::Result<Self> {
        let mut magic = [0u8; 8];
        r.read_exact(&mut magic)?;
        if &magic != MAGIC {
            return Err(bad_data("not a tprobe v1 file (bad magic)"));
        }
        let mut len = [0u8; 4];
        r.read_exact(&mut len)?;
        let hlen = u32::from_le_bytes(len);
        if hlen > MAX_HEADER_LEN {
            return Err(bad_data(format!(
                "header length {hlen} exceeds the {MAX_HEADER_LEN} byte limit"
            )));
        }
        let hjson = read_exact_vec(&mut r, u64::from(hlen))?;
        let header: Header = serde_json::from_slice(&hjson).map_err(|e| bad_data(e.to_string()))?;
        drop(hjson);
        if header.format != FORMAT_NAME {
            return Err(bad_data(format!("unsupported format {:?}", header.format)));
        }
        if header.sections.len() != SECTION_NAMES.len() {
            return Err(bad_data("unexpected section table"));
        }
        header.check_dims()?;
        let n = header.columns() as u64;
        let height = header.height() as u64;
        let mut dec = zstd::bulk::Decompressor::new()?;
        // Sections are decoded one at a time; each raw buffer is dropped as
        // soon as its typed array exists.
        let mut sec = |idx: usize, expect: Option<u64>| -> io::Result<Vec<u8>> {
            let info = &header.sections[idx];
            if info.name != SECTION_NAMES[idx] {
                return Err(bad_data(format!(
                    "section {:?}, expected {:?}",
                    info.name, SECTION_NAMES[idx]
                )));
            }
            read_section(&mut r, &mut dec, info, expect)
        };
        let alt = unpack_lanes(&sec(0, Some(n * 4))?, f32::from_le_bytes)?;
        let riverless_alt = unpack_lanes(&sec(1, Some(n * 4))?, f32::from_le_bytes)?;
        let water_level = unpack_lanes(&sec(2, Some(n * 4))?, f32::from_le_bytes)?;
        let warp_factor = unpack_lanes(&sec(3, Some(n * 4))?, f32::from_le_bytes)?;
        let top_kind = sec(4, Some(n))?;
        let flags = sec(5, Some(n))?;
        let ground_top = unpack_lanes(&sec(6, Some(n * 2))?, i16::from_le_bytes)?;
        let water_top = unpack_lanes(&sec(7, Some(n * 2))?, i16::from_le_bytes)?;
        let liquid_depth = unpack_lanes(&sec(8, Some(n * 2))?, u16::from_le_bytes)?;
        let run_counts = unpack_lanes(&sec(9, Some(n * 2))?, u16::from_le_bytes)?;
        let total_runs: u64 = run_counts.iter().map(|&c| u64::from(c)).sum();
        if total_runs > n * height {
            return Err(bad_data(format!(
                "run_counts sum to {total_runs}, more than {n} columns x {height} blocks"
            )));
        }
        let run_class = sec(10, Some(total_runs))?;
        let run_len = unpack_lanes(&sec(11, Some(total_runs * 2))?, u16::from_le_bytes)?;
        let sim_csv = String::from_utf8(sec(12, None)?).map_err(|e| bad_data(e.to_string()))?;
        let sites: Vec<SiteRec> =
            serde_json::from_slice(&sec(13, None)?).map_err(|e| bad_data(e.to_string()))?;
        let d = Self {
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
        };
        d.validate()?;
        Ok(d)
    }

    pub fn write_file(&self, path: &std::path::Path) -> io::Result<()> {
        let f = std::fs::File::create(path)?;
        self.write_to(io::BufWriter::new(f))
    }

    pub fn read_file(path: &std::path::Path) -> io::Result<Self> {
        let f = std::fs::File::open(path)?;
        Self::read_from(io::BufReader::new(f))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a small synthetic dump: a 3x2 box, z in [10, 20).
    fn sample() -> Dump {
        let (nx, ny) = (3usize, 2usize);
        let (zmin, zmax) = (10, 20);
        let mut run_counts = Vec::new();
        let mut run_class = Vec::new();
        let mut run_len = Vec::new();
        let (mut top_kind, mut flags, mut gt, mut wt, mut ld) =
            (vec![], vec![], vec![], vec![], vec![]);
        for i in 0..nx * ny {
            // Ground, then water, then air; column 4 has a cave, column 1 a
            // structure block at the very top.
            let ground = 4 + i as u16;
            let mut cls: Vec<u8> = Vec::new();
            for z in 0..10u16 {
                cls.push(if z < ground {
                    if i == 4 && z == 2 {
                        class::AIR
                    } else {
                        class::GROUND
                    }
                } else if z < 6 + ground.min(4) {
                    class::LIQUID
                } else if i == 1 && z == 9 {
                    class::STRUCTURE
                } else {
                    class::AIR
                });
            }
            let runs = rle(cls);
            let s = summarize(zmin, &runs);
            run_counts.push(runs.len() as u16);
            for (c, n) in runs {
                run_class.push(c);
                run_len.push(n);
            }
            top_kind.push(if s.ground_top == NO_Z { 255 } else { 3 });
            flags.push(s.flags);
            gt.push(s.ground_top);
            wt.push(s.water_top);
            ld.push(s.liquid_depth);
        }
        let n = nx * ny;
        Dump {
            header: Header {
                format: FORMAT_NAME.into(),
                box_xy: [100, 200, 103, 202],
                zmin,
                zmax,
                nx: nx as u32,
                ny: ny as u32,
                seed: 7,
                path: "fast".into(),
                calendar: None,
                engine_commit: "abc".into(),
                assets: [("x.bin".to_string(), "00".to_string())]
                    .into_iter()
                    .collect(),
                class_codes: Header::class_codes(),
                block_kinds: BTreeMap::new(),
                stats: [("columns".to_string(), n as u64)].into_iter().collect(),
                client: None,
                sections: vec![],
            },
            alt: (0..n).map(|i| 12.5 + i as f32 * 0.25).collect(),
            riverless_alt: (0..n).map(|i| 12.0 - i as f32).collect(),
            water_level: (0..n)
                .map(|i| if i % 2 == 0 { 15.0 } else { f32::NAN })
                .collect(),
            warp_factor: (0..n).map(|i| i as f32 / 7.0).collect(),
            top_kind,
            flags,
            ground_top: gt,
            water_top: wt,
            liquid_depth: ld,
            run_counts,
            run_class,
            run_len,
            sim_csv: "cx;cy;alt\n3;6;12.5\n".into(),
            sites: vec![SiteRec {
                source: "world_site".into(),
                id: None,
                name: Some("Test".into()),
                kind: Some("Castle".into()),
                wx: 5,
                wy: -6,
                radius: Some(30.0),
            }],
        }
    }

    fn bytes(d: &Dump) -> Vec<u8> {
        let mut v = Vec::new();
        d.write_to(&mut v).unwrap();
        v
    }

    #[test]
    fn round_trip_is_lossless() {
        let d = sample();
        let b = bytes(&d);
        let r = Dump::read_from(&b[..]).unwrap();
        // NaN != NaN, so compare f32 arrays bitwise and the rest structurally.
        let bits = |v: &[f32]| v.iter().map(|x| x.to_bits()).collect::<Vec<_>>();
        assert_eq!(bits(&r.alt), bits(&d.alt));
        assert_eq!(bits(&r.water_level), bits(&d.water_level));
        assert_eq!(bits(&r.warp_factor), bits(&d.warp_factor));
        assert_eq!(bits(&r.riverless_alt), bits(&d.riverless_alt));
        assert_eq!(r.run_counts, d.run_counts);
        assert_eq!(r.run_class, d.run_class);
        assert_eq!(r.run_len, d.run_len);
        assert_eq!(r.ground_top, d.ground_top);
        assert_eq!(r.water_top, d.water_top);
        assert_eq!(r.liquid_depth, d.liquid_depth);
        assert_eq!(r.flags, d.flags);
        assert_eq!(r.top_kind, d.top_kind);
        assert_eq!(r.sim_csv, d.sim_csv);
        assert_eq!(r.sites, d.sites);
        assert_eq!(r.header.box_xy, d.header.box_xy);
        assert_eq!(r.header.assets, d.header.assets);
        assert_eq!(r.header.sections.len(), 14);
    }

    #[test]
    fn client_info_round_trips_and_fast_header_has_no_client_key() {
        let mut d = sample();
        let b = bytes(&d);
        let hl = u32::from_le_bytes(b[8..12].try_into().unwrap()) as usize;
        let hjson = std::str::from_utf8(&b[12..12 + hl]).unwrap();
        assert!(!hjson.contains("\"client\""), "fast dumps keep their bytes");
        d.header.path = "client".into();
        d.header.client = Some(ClientInfo {
            view_distance: 24,
            chunks_total: 10,
            chunks_streamed: 9,
            missing_chunks: vec![[3, 4]],
            tiles: vec![TileInfo {
                chunks: [0, 0, 4, 1],
                goto: [10, 20, 30],
                landing: Some([10.5, 20.5, 31.0]),
                streamed: 9,
                total: 10,
                secs: 1.5,
            }],
        });
        let r = Dump::read_from(&bytes(&d)[..]).unwrap();
        assert_eq!(r.header.client, d.header.client);
        assert_eq!(r.header.path, "client");
        assert!((r.header.client.unwrap().streamed_pct() - 90.0).abs() < 1e-9);
    }

    #[test]
    fn output_is_deterministic() {
        let d = sample();
        assert_eq!(bytes(&d), bytes(&d));
        // Re-writing what was read yields the same bytes too.
        let b = bytes(&d);
        let r = Dump::read_from(&b[..]).unwrap();
        assert_eq!(bytes(&r), b);
    }

    #[test]
    fn rejects_bad_magic_and_truncation() {
        let b = bytes(&sample());
        let mut bad = b.clone();
        bad[0] = b'X';
        assert!(Dump::read_from(&bad[..]).is_err());
        assert!(Dump::read_from(&b[..b.len() - 5]).is_err());
        assert!(Dump::read_from(&b[..6]).is_err());
    }

    #[test]
    fn validate_catches_inconsistent_runs() {
        let mut d = sample();
        d.run_len[0] += 1;
        assert!(d.validate().is_err());
        assert!(d.write_to(Vec::new()).is_err());
    }

    #[test]
    fn rle_and_summary() {
        use class::*;
        // z 10..12 ground, 12 air (cave), 13..15 ground, 15..17 water, air, structure.
        let cls = [
            GROUND, GROUND, AIR, GROUND, GROUND, LIQUID, LIQUID, AIR, AIR, STRUCTURE,
        ];
        let runs = rle(cls);
        assert_eq!(runs, vec![
            (GROUND, 2),
            (AIR, 1),
            (GROUND, 2),
            (LIQUID, 2),
            (AIR, 2),
            (STRUCTURE, 1)
        ]);
        let s = summarize(10, &runs);
        assert_eq!(s.ground_top, 14);
        assert_eq!(s.water_top, 16);
        assert_eq!(s.liquid_depth, 2);
        assert_ne!(s.flags & flag::VOID, 0);
        assert_ne!(s.flags & flag::LIQUID, 0);
        assert_ne!(s.flags & flag::STRUCTURE, 0);
        assert_ne!(s.flags & flag::CLIPPED_TOP, 0, "ends in a solid block");
        // Plain air above ground: no void, no clip.
        let s = summarize(0, &rle([GROUND, GROUND, AIR]));
        assert_eq!((s.ground_top, s.water_top), (1, NO_Z));
        assert_eq!(s.flags, 0);
        // Sea bed under water is not a void.
        let s = summarize(0, &rle([GROUND, LIQUID, LIQUID, AIR]));
        assert_eq!(s.flags & flag::VOID, 0);
        // No ground at all.
        assert_eq!(summarize(0, &rle([AIR, AIR])).ground_top, NO_Z);
    }

    #[test]
    fn class_at_and_index() {
        let d = sample();
        let off = d.run_offsets();
        let idx = d.col_index(101, 201).unwrap();
        assert_eq!(idx, 4);
        assert_eq!(d.class_at(&off, idx, 10), Some(class::GROUND));
        assert_eq!(d.class_at(&off, idx, 12), Some(class::AIR), "the cave");
        assert_eq!(d.class_at(&off, idx, 9), None);
        assert_eq!(d.class_at(&off, idx, 20), None);
        assert!(d.col_index(103, 200).is_none());
    }

    #[test]
    fn lane_packing_round_trips() {
        let v: Vec<u32> = (0..7).map(|i| 0x0102_0304u32.wrapping_mul(i + 1)).collect();
        let packed = pack_lanes(&v, u32::to_le_bytes);
        // All byte-0s first: lane 0 of element 0 is the low byte.
        assert_eq!(packed[0], v[0].to_le_bytes()[0]);
        assert_eq!(packed[v.len()], v[0].to_le_bytes()[1]);
        assert_eq!(unpack_lanes(&packed, u32::from_le_bytes).unwrap(), v);
        assert!(unpack_lanes::<4, u32>(&packed[1..], u32::from_le_bytes).is_err());
    }

    // ---------------------------------------------------------- write path

    /// The pre-optimisation writer, kept verbatim as the reference for the
    /// byte-identity tests: every raw buffer built at once, `shuffle` into a
    /// second buffer, sequential `zstd::bulk::compress`.
    fn legacy_write<W: Write>(d: &Dump, mut out: W) {
        fn shuffle(bytes: &[u8], width: usize) -> Vec<u8> {
            if width == 1 {
                return bytes.to_vec();
            }
            let n = bytes.len() / width;
            let mut out = vec![0u8; bytes.len()];
            for (i, chunk) in bytes.chunks_exact(width).enumerate() {
                for (b, &v) in chunk.iter().enumerate() {
                    out[b * n + i] = v;
                }
            }
            out
        }
        let f32s = |v: &[f32]| {
            shuffle(
                &v.iter().flat_map(|x| x.to_le_bytes()).collect::<Vec<_>>(),
                4,
            )
        };
        let i16s = |v: &[i16]| {
            shuffle(
                &v.iter().flat_map(|x| x.to_le_bytes()).collect::<Vec<_>>(),
                2,
            )
        };
        let u16s = |v: &[u16]| {
            shuffle(
                &v.iter().flat_map(|x| x.to_le_bytes()).collect::<Vec<_>>(),
                2,
            )
        };
        let sites_json = serde_json::to_vec(&d.sites).unwrap();
        let raw: [Vec<u8>; 14] = [
            f32s(&d.alt),
            f32s(&d.riverless_alt),
            f32s(&d.water_level),
            f32s(&d.warp_factor),
            d.top_kind.clone(),
            d.flags.clone(),
            i16s(&d.ground_top),
            i16s(&d.water_top),
            u16s(&d.liquid_depth),
            u16s(&d.run_counts),
            d.run_class.clone(),
            u16s(&d.run_len),
            d.sim_csv.clone().into_bytes(),
            sites_json,
        ];
        let mut comp = Vec::new();
        let mut sections = Vec::new();
        for (name, r) in SECTION_NAMES.iter().zip(raw.iter()) {
            let c = zstd::bulk::compress(r, ZSTD_LEVEL).unwrap();
            sections.push(SectionInfo {
                name: (*name).to_string(),
                raw_len: r.len() as u64,
                comp_len: c.len() as u64,
            });
            comp.push(c);
        }
        let mut header = d.header.clone();
        header.sections = sections;
        let hjson = serde_json::to_vec(&header).unwrap();
        out.write_all(MAGIC).unwrap();
        out.write_all(&(hjson.len() as u32).to_le_bytes()).unwrap();
        out.write_all(&hjson).unwrap();
        for c in &comp {
            out.write_all(c).unwrap();
        }
    }

    fn legacy_bytes(d: &Dump) -> Vec<u8> {
        let mut v = Vec::new();
        legacy_write(d, &mut v);
        v
    }

    /// Counts what is written to it without keeping it.
    struct Sink(u64);

    impl Write for Sink {
        fn write(&mut self, b: &[u8]) -> io::Result<usize> {
            self.0 += b.len() as u64;
            Ok(b.len())
        }

        fn flush(&mut self) -> io::Result<()> { Ok(()) }
    }

    /// Manual write-path benchmark. Run each variant in its own process under
    /// `/usr/bin/time -l` to compare peak RSS (the read is shared):
    /// `TPROBE_BENCH_IN=big.tprobe terrain-probe-test bench_repack_new
    /// --ignored --nocapture --exact` vs `bench_repack_legacy`.
    fn bench_repack(legacy: bool) {
        let path = std::env::var("TPROBE_BENCH_IN").expect("set TPROBE_BENCH_IN");
        let t = std::time::Instant::now();
        let d = Dump::read_file(std::path::Path::new(&path)).unwrap();
        eprintln!("read: {:.2}s", t.elapsed().as_secs_f32());
        let t = std::time::Instant::now();
        let mut sink = Sink(0);
        if legacy {
            legacy_write(&d, &mut sink);
        } else {
            d.write_to(&mut sink).unwrap();
        }
        eprintln!(
            "write ({}): {:.2}s, {} bytes",
            if legacy { "legacy" } else { "new" },
            t.elapsed().as_secs_f32(),
            sink.0
        );
    }

    #[test]
    #[ignore = "manual benchmark, needs TPROBE_BENCH_IN"]
    fn bench_repack_new() { bench_repack(false) }

    #[test]
    #[ignore = "manual benchmark, needs TPROBE_BENCH_IN"]
    fn bench_repack_legacy() { bench_repack(true) }

    #[test]
    #[ignore = "manual benchmark, needs TPROBE_BENCH_IN"]
    fn bench_read_only() {
        let path = std::env::var("TPROBE_BENCH_IN").expect("set TPROBE_BENCH_IN");
        let d = Dump::read_file(std::path::Path::new(&path)).unwrap();
        eprintln!("columns {}", d.alt.len());
    }

    /// Small deterministic xorshift64* generator for the seeded tests.
    struct Rng(u64);

    impl Rng {
        fn next(&mut self) -> u64 {
            self.0 ^= self.0 >> 12;
            self.0 ^= self.0 << 25;
            self.0 ^= self.0 >> 27;
            self.0.wrapping_mul(0x2545_F491_4F6C_DD1D)
        }

        fn below(&mut self, n: u64) -> u64 { self.next() % n }
    }

    /// A seeded pseudo-random dump of `nx x ny` columns and `h` blocks, with
    /// every class, NaN floats and non-trivial sections.
    fn random_dump(seed: u64, nx: usize, ny: usize, h: usize) -> Dump {
        let mut rng = Rng(seed | 1);
        let n = nx * ny;
        let zmin = rng.below(200) as i32 - 100;
        let (mut run_counts, mut run_class, mut run_len) = (vec![], vec![], vec![]);
        let (mut flags, mut gt, mut wt, mut ld, mut tk) = (vec![], vec![], vec![], vec![], vec![]);
        for _ in 0..n {
            let mut cls = Vec::with_capacity(h);
            let mut c = class::GROUND;
            for _ in 0..h {
                if rng.below(12) == 0 {
                    c = rng.below(6) as u8;
                }
                cls.push(c);
            }
            let runs = rle(cls);
            let s = summarize(zmin, &runs);
            run_counts.push(runs.len() as u16);
            for (c, l) in runs {
                run_class.push(c);
                run_len.push(l);
            }
            flags.push(s.flags);
            gt.push(s.ground_top);
            wt.push(s.water_top);
            ld.push(s.liquid_depth);
            tk.push(rng.below(256) as u8);
        }
        let fl = |rng: &mut Rng| {
            (0..n)
                .map(|_| match rng.below(9) {
                    0 => f32::NAN,
                    _ => (rng.below(100_000) as f32) / 37.0 - 500.0,
                })
                .collect::<Vec<f32>>()
        };
        let mut d = sample();
        d.header.box_xy = [10, 20, 10 + nx as i32, 20 + ny as i32];
        d.header.nx = nx as u32;
        d.header.ny = ny as u32;
        d.header.zmin = zmin;
        d.header.zmax = zmin + h as i32;
        d.header.stats = [("columns".to_string(), n as u64)].into_iter().collect();
        d.alt = fl(&mut rng);
        d.riverless_alt = fl(&mut rng);
        d.water_level = fl(&mut rng);
        d.warp_factor = fl(&mut rng);
        d.top_kind = tk;
        d.flags = flags;
        d.ground_top = gt;
        d.water_top = wt;
        d.liquid_depth = ld;
        d.run_counts = run_counts;
        d.run_class = run_class;
        d.run_len = run_len;
        d.sim_csv = (0..40)
            .map(|i| format!("{i};{};{}\n", rng.next(), rng.below(9)))
            .collect();
        d.validate().unwrap();
        d
    }

    fn sha256_hex(b: &[u8]) -> String {
        use sha2::{Digest, Sha256};
        Sha256::digest(b)
            .iter()
            .map(|x| format!("{x:02x}"))
            .collect()
    }

    /// Pinned output of the pre-optimisation writer for [`sample`] (its flags
    /// fixed to the values the writer was pinned with, so later changes to
    /// `summarize` cannot move the golden).
    #[test]
    fn golden_hash_of_the_v1_bytes_is_stable() {
        let mut d = sample();
        d.flags = vec![24, 24, 24, 24, 28, 24];
        let b = bytes(&d);
        assert_eq!(b.len(), 1394);
        assert_eq!(
            sha256_hex(&b),
            "7b70d14dd08c0eaae3cf49278d4bbebf3ccbe5f222fca4285a9c8465b0511406"
        );
    }

    #[test]
    fn new_writer_is_byte_identical_to_the_legacy_writer() {
        let mut dumps = vec![sample()];
        for (seed, nx, ny, h) in [
            (1, 1, 1, 1),
            (2, 37, 29, 64),
            (3, 100, 3, 300),
            (4, 5, 211, 17),
        ] {
            dumps.push(random_dump(seed, nx, ny, h));
        }
        for (i, d) in dumps.iter().enumerate() {
            assert!(
                bytes(d) == legacy_bytes(d),
                "dump {i}: bytes differ from the legacy writer"
            );
            // And the reader gets back exactly what was written.
            let r = Dump::read_from(&bytes(d)[..]).unwrap();
            assert_eq!(r.run_len, d.run_len, "dump {i}");
            assert_eq!(r.sim_csv, d.sim_csv, "dump {i}");
        }
    }

    // ----------------------------------------------------- malformed input

    /// Re-serialise the header of `file` after `edit`, keeping the body.
    fn with_header(file: &[u8], edit: impl FnOnce(&mut Header)) -> Vec<u8> {
        let hl = u32::from_le_bytes(file[8..12].try_into().unwrap()) as usize;
        let mut h: Header = serde_json::from_slice(&file[12..12 + hl]).unwrap();
        edit(&mut h);
        let hj = serde_json::to_vec(&h).unwrap();
        let mut out = file[..8].to_vec();
        out.extend_from_slice(&(hj.len() as u32).to_le_bytes());
        out.extend_from_slice(&hj);
        out.extend_from_slice(&file[12 + hl..]);
        out
    }

    /// Largest single allocation any malformed-input read may make. The file
    /// under test is a few KB, so anything near this means the reader trusted a
    /// declared length.
    const BOUND: usize = 4 << 20;

    fn read_bounded(data: &[u8]) -> (io::Result<Dump>, crate::test_alloc::Usage) {
        crate::test_alloc::measure(|| Dump::read_from(data))
    }

    #[test]
    fn huge_declared_lengths_are_errors_not_allocations() {
        let good = bytes(&random_dump(9, 30, 20, 40));
        let tweaks: Vec<(&str, Vec<u8>)> = vec![
            ("header_len u32::MAX", {
                let mut b = good.clone();
                b[8..12].copy_from_slice(&u32::MAX.to_le_bytes());
                b
            }),
            ("header_len just over the cap", {
                let mut b = good.clone();
                b[8..12].copy_from_slice(&(MAX_HEADER_LEN + 1).to_le_bytes());
                b
            }),
            ("header_len at the cap on a short file", {
                let mut b = good.clone();
                b[8..12].copy_from_slice(&MAX_HEADER_LEN.to_le_bytes());
                b
            }),
            (
                "comp_len u64::MAX",
                with_header(&good, |h| h.sections[0].comp_len = u64::MAX),
            ),
            (
                "raw_len u64::MAX",
                with_header(&good, |h| h.sections[0].raw_len = u64::MAX),
            ),
            (
                "raw_len 4 GB on a text section",
                with_header(&good, |h| {
                    h.sections[12].raw_len = 4 << 30;
                }),
            ),
            (
                "raw_len 4 GB on run_len",
                with_header(&good, |h| {
                    h.sections[11].raw_len = 4 << 30;
                }),
            ),
            (
                "comp_len 4 GB",
                with_header(&good, |h| h.sections[5].comp_len = 4 << 30),
            ),
            (
                "nx = ny = i32::MAX columns",
                with_header(&good, |h| {
                    h.box_xy = [0, 0, i32::MAX, i32::MAX];
                    h.nx = i32::MAX as u32;
                    h.ny = i32::MAX as u32;
                }),
            ),
            (
                "20000 x 20000 box on a tiny file",
                with_header(&good, |h| {
                    h.box_xy = [0, 0, 20_000, 20_000];
                    h.nx = 20_000;
                    h.ny = 20_000;
                }),
            ),
            (
                "nx not matching the box",
                with_header(&good, |h| h.nx = u32::MAX),
            ),
            (
                "z range overflowing i32",
                with_header(&good, |h| {
                    h.zmin = i32::MIN;
                    h.zmax = i32::MAX;
                }),
            ),
            (
                "inverted z range",
                with_header(&good, |h| std::mem::swap(&mut h.zmin, &mut h.zmax)),
            ),
            (
                "z range beyond i16",
                with_header(&good, |h| h.zmax = 40_000),
            ),
        ];
        for (what, data) in tweaks {
            let (res, u) = read_bounded(&data);
            assert!(res.is_err(), "{what}: expected an error");
            assert!(
                u.biggest < BOUND && u.peak < 8 * BOUND,
                "{what}: allocated {u:?} while rejecting a {} byte file",
                data.len()
            );
        }
    }

    #[test]
    fn seeded_corruption_and_truncation_never_panic_or_balloon() {
        let good = bytes(&random_dump(11, 40, 30, 50));
        let mut rng = Rng(0x5eed_1234_abcd_0001);
        let (mut errs, mut oks) = (0, 0);
        for round in 0..600 {
            let mut data = good.clone();
            match round % 4 {
                // Random byte flips anywhere.
                0 => {
                    for _ in 0..=rng.below(5) {
                        let i = rng.below(data.len() as u64) as usize;
                        data[i] ^= 1 << rng.below(8);
                    }
                },
                // Truncation at a random point.
                1 => data.truncate(rng.below(data.len() as u64) as usize),
                // A window of 0xFF / 0x00 (declared-length style damage), in
                // the first 400 bytes (length prefix + header JSON) half the
                // time.
                2 => {
                    let span = if rng.below(2) == 0 {
                        400.min(data.len())
                    } else {
                        data.len()
                    };
                    let i = rng.below(span as u64) as usize;
                    let fill = if rng.below(2) == 0 { 0xFF } else { 0x00 };
                    for b in data.iter_mut().skip(i).take(1 + rng.below(8) as usize) {
                        *b = fill;
                    }
                },
                // Flips and then a truncation.
                _ => {
                    for _ in 0..3 {
                        let i = rng.below(data.len() as u64) as usize;
                        data[i] = rng.below(256) as u8;
                    }
                    data.truncate(1 + rng.below(data.len() as u64 - 1) as usize);
                },
            }
            let (res, u) = read_bounded(&data);
            assert!(
                u.biggest < BOUND && u.peak < 8 * BOUND,
                "round {round}: allocated {u:?} reading a {} byte corrupt file",
                data.len()
            );
            match res {
                Ok(d) => {
                    d.validate()
                        .expect("an accepted dump is internally consistent");
                    oks += 1;
                },
                Err(_) => errs += 1,
            }
        }
        assert!(
            errs > 400,
            "only {errs} of 600 corrupt files were rejected ({oks} accepted)"
        );
    }
}

#[cfg(test)]
#[path = "format_edge_tests.rs"]
pub(crate) mod edge_tests;
