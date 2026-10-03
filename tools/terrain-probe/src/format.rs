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
    collections::BTreeMap,
    io::{self, Read, Write},
};

use serde::{Deserialize, Serialize};

pub const MAGIC: &[u8; 8] = b"TPROBE1\n";
pub const FORMAT_NAME: &str = "tprobe v1";
pub const ZSTD_LEVEL: i32 = 3;

/// Block classes (one byte per block, run-length encoded per column).
pub mod class {
    /// Air (non-solid, non-liquid, no sprite).
    pub const AIR: u8 = 0;
    /// Natural ground: Rock, WeakRock, GlowingRock, Grass, Snow, Earth, Sand,
    /// Ice.
    pub const GROUND: u8 = 1;
    /// Liquid (water, lava).
    pub const LIQUID: u8 = 2;
    /// Not loaded / not generated (client path only).
    pub const UNLOADED: u8 = 3;
    /// Any other solid, non-sprite block: structures, wood, leaves.
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
    pub sections: Vec<SectionInfo>,
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

    pub fn height(&self) -> usize { (self.zmax - self.zmin) as usize }
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
pub fn summarize(zmin: i32, runs: &[(u8, u16)]) -> ColSummary {
    let mut z = zmin;
    let mut ground_top = i32::MIN;
    let mut water_top = i32::MIN;
    let mut flags = 0u8;
    // Pass 1: tops and the plain flags.
    for &(c, n) in runs {
        let top = z + i32::from(n) - 1;
        match c {
            class::GROUND => ground_top = top,
            class::LIQUID => {
                water_top = top;
                flags |= flag::LIQUID;
            },
            class::STRUCTURE => flags |= flag::STRUCTURE,
            class::SPRITE => flags |= flag::SPRITE,
            _ => {},
        }
        z += i32::from(n);
    }
    if let Some(&(c, _)) = runs.last()
        && c != class::AIR
        && c != class::SPRITE
        && c != class::UNLOADED
    {
        flags |= flag::CLIPPED_TOP;
    }
    // Pass 2: voids and liquid depth, relative to the ground top.
    let mut liquid_depth = 0u32;
    let mut z = zmin;
    for &(c, n) in runs {
        let top = z + i32::from(n) - 1;
        let non_solid = matches!(c, class::AIR | class::LIQUID | class::SPRITE);
        if ground_top != i32::MIN {
            if non_solid && top < ground_top {
                flags |= flag::VOID;
            }
            if c == class::LIQUID && z > ground_top {
                liquid_depth += u32::from(n);
            }
        }
        z += i32::from(n);
    }
    ColSummary {
        ground_top: if ground_top == i32::MIN {
            NO_Z
        } else {
            ground_top as i16
        },
        water_top: if water_top == i32::MIN {
            NO_Z
        } else {
            water_top as i16
        },
        liquid_depth: liquid_depth.min(u32::from(u16::MAX)) as u16,
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

fn unshuffle(bytes: &[u8], width: usize) -> Vec<u8> {
    if width == 1 {
        return bytes.to_vec();
    }
    let n = bytes.len() / width;
    let mut out = vec![0u8; bytes.len()];
    for i in 0..n {
        for b in 0..width {
            out[i * width + b] = bytes[b * n + i];
        }
    }
    out
}

fn pack_f32(v: &[f32]) -> Vec<u8> {
    let raw: Vec<u8> = v.iter().flat_map(|x| x.to_le_bytes()).collect();
    shuffle(&raw, 4)
}

fn pack_i16(v: &[i16]) -> Vec<u8> {
    let raw: Vec<u8> = v.iter().flat_map(|x| x.to_le_bytes()).collect();
    shuffle(&raw, 2)
}

fn pack_u16(v: &[u16]) -> Vec<u8> {
    let raw: Vec<u8> = v.iter().flat_map(|x| x.to_le_bytes()).collect();
    shuffle(&raw, 2)
}

fn bad_data(m: impl Into<String>) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, m.into())
}

fn unpack<const N: usize, T>(b: &[u8], from: fn([u8; N]) -> T) -> io::Result<Vec<T>> {
    if !b.len().is_multiple_of(N) {
        return Err(bad_data(format!(
            "section length {} is not a multiple of {N}",
            b.len()
        )));
    }
    let raw = unshuffle(b, N);
    let (items, _) = raw.as_chunks::<N>();
    Ok(items.iter().map(|c| from(*c)).collect())
}

fn unpack_f32(b: &[u8]) -> io::Result<Vec<f32>> { unpack(b, f32::from_le_bytes) }

fn unpack_i16(b: &[u8]) -> io::Result<Vec<i16>> { unpack(b, i16::from_le_bytes) }

fn unpack_u16(b: &[u8]) -> io::Result<Vec<u16>> { unpack(b, u16::from_le_bytes) }

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

impl Dump {
    /// Serialise to `w`. `self.header.sections` is rebuilt; everything else in
    /// the header is written as given.
    pub fn write_to<W: Write>(&self, mut w: W) -> io::Result<()> {
        self.validate()?;
        let sites_json = serde_json::to_vec(&self.sites).map_err(io::Error::other)?;
        let raw: [Vec<u8>; 14] = [
            pack_f32(&self.alt),
            pack_f32(&self.riverless_alt),
            pack_f32(&self.water_level),
            pack_f32(&self.warp_factor),
            self.top_kind.clone(),
            self.flags.clone(),
            pack_i16(&self.ground_top),
            pack_i16(&self.water_top),
            pack_u16(&self.liquid_depth),
            pack_u16(&self.run_counts),
            self.run_class.clone(),
            pack_u16(&self.run_len),
            self.sim_csv.clone().into_bytes(),
            sites_json,
        ];
        let mut comp = Vec::with_capacity(raw.len());
        let mut sections = Vec::with_capacity(raw.len());
        for (name, r) in SECTION_NAMES.iter().zip(raw.iter()) {
            let c = zstd::bulk::compress(r, ZSTD_LEVEL)?;
            sections.push(SectionInfo {
                name: (*name).to_string(),
                raw_len: r.len() as u64,
                comp_len: c.len() as u64,
            });
            comp.push(c);
        }
        let mut header = self.header.clone();
        header.sections = sections;
        let hjson = serde_json::to_vec(&header).map_err(io::Error::other)?;
        w.write_all(MAGIC)?;
        w.write_all(&(hjson.len() as u32).to_le_bytes())?;
        w.write_all(&hjson)?;
        for c in &comp {
            w.write_all(c)?;
        }
        w.flush()
    }

    pub fn read_from<R: Read>(mut r: R) -> io::Result<Self> {
        let mut magic = [0u8; 8];
        r.read_exact(&mut magic)?;
        if &magic != MAGIC {
            return Err(bad_data("not a tprobe v1 file (bad magic)"));
        }
        let mut len = [0u8; 4];
        r.read_exact(&mut len)?;
        let mut hjson = vec![0u8; u32::from_le_bytes(len) as usize];
        r.read_exact(&mut hjson)?;
        let header: Header = serde_json::from_slice(&hjson).map_err(|e| bad_data(e.to_string()))?;
        if header.format != FORMAT_NAME {
            return Err(bad_data(format!("unsupported format {:?}", header.format)));
        }
        if header.sections.len() != SECTION_NAMES.len() {
            return Err(bad_data("unexpected section table"));
        }
        let mut secs: Vec<Vec<u8>> = Vec::with_capacity(SECTION_NAMES.len());
        for (info, name) in header.sections.iter().zip(SECTION_NAMES) {
            if info.name != name {
                return Err(bad_data(format!(
                    "section {:?}, expected {name:?}",
                    info.name
                )));
            }
            let mut comp = vec![0u8; info.comp_len as usize];
            r.read_exact(&mut comp)?;
            let raw = zstd::bulk::decompress(&comp, info.raw_len as usize)?;
            if raw.len() as u64 != info.raw_len {
                return Err(bad_data(format!("section {name}: wrong decompressed size")));
            }
            secs.push(raw);
        }
        let mut it = secs.into_iter();
        let mut next = || it.next().ok_or_else(|| bad_data("missing section"));
        let alt = unpack_f32(&next()?)?;
        let riverless_alt = unpack_f32(&next()?)?;
        let water_level = unpack_f32(&next()?)?;
        let warp_factor = unpack_f32(&next()?)?;
        let top_kind = next()?;
        let flags = next()?;
        let ground_top = unpack_i16(&next()?)?;
        let water_top = unpack_i16(&next()?)?;
        let liquid_depth = unpack_u16(&next()?)?;
        let run_counts = unpack_u16(&next()?)?;
        let run_class = next()?;
        let run_len = unpack_u16(&next()?)?;
        let sim_csv = String::from_utf8(next()?).map_err(|e| bad_data(e.to_string()))?;
        let sites: Vec<SiteRec> =
            serde_json::from_slice(&next()?).map_err(|e| bad_data(e.to_string()))?;
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
    fn shuffle_round_trips() {
        let b: Vec<u8> = (0..24).collect();
        for w in [1, 2, 4, 8] {
            assert_eq!(unshuffle(&shuffle(&b, w), w), b);
        }
    }
}
