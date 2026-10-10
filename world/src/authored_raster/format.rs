//! Binary tile codec of the authored raster layers (tile format version 1).
//!
//! One tile is 256 x 256 cells of 1 m, one cell per block column, rows from
//! south to north, `x` fastest. Layout (little-endian):
//!
//! | offset | field | value |
//! |---:|---|---|
//! | 0 | magic | `b"XART"` |
//! | 4 | version | `u16` = 1 |
//! | 6 | layer code | `u8` (see [`LayerKind::code`]) |
//! | 7 | flags | `u8` = 0 |
//! | 8 | tile size | `u16` = 256 |
//! | 10 | reserved | `u16` = 0 |
//! | 12 | origin | `i32` x, `i32` y (wpos of cell 0,0) |
//! | 20 | base | `i32` centimetres (water); `0` (ground) |
//! | 24 | payload length | `u32` |
//! | 28 | payload | zstd of the layer's planes ([`LayerKind::payload_len`]) |
//!
//! The payload layout is a function of the layer code alone:
//!
//! * **Water** (code 1): two `[u16; 65536]` planes, surface then bed. A stored
//!   value of [`NONE`] means "not authored"; any other value `v` means the
//!   altitude `base + v` centimetres in the engine's block-z frame.
//! * **Ground** (code 2, Stage 2): a `ground` plane of absolute centimetres
//!   (`i32`) followed by a `weight` plane (`u8`). On disk the ground plane is
//!   *filtered* so smooth slopes compress like flats (see [`encode_ground`]):
//!   per row, each cell stores the wrapping `i32` difference to the last
//!   present cell of that row (the row starts from 0); a cell whose weight is 0
//!   is "not authored" and must store difference 0 (the canonical form: any
//!   other value is a decode error, so equal rasters always give equal bytes);
//!   the 65536 differences are then **byte-shuffled** (all byte 0s, then all
//!   byte 1s, 2s, 3s). Weight 255 = exact ground; 1..=254 = a blend cell toward
//!   the engine terrain (Stage 2b); 0 = none. The header's `base` must be 0.
//!
//! **Adding a layer** (a lava layer) means a new `LayerKind` variant with a
//! new layer code and its own payload, under the *same* tile format version:
//! an older engine meets the new variant first in the manifest (an unknown
//! enum variant is a parse error) and, should a tile reach it anyway, rejects
//! the unknown layer code with an explicit error -- never a silent
//! misreading. The tile `version` only moves if this header or a layer's
//! value encoding changes.
//!
//! Decoding never trusts a length it read: the payload length must equal the
//! rest of the file and the decompressed size is fixed by the layer, so a
//! corrupt tile is an error, never an oversized allocation.

use std::io::Read;
use vek::Vec2;

/// Edge length of a tile in cells (= metres = block columns).
pub const TILE_SIZE: i32 = 256;
/// Cells per tile.
pub const TILE_CELLS: usize = (TILE_SIZE * TILE_SIZE) as usize;
/// Stored value meaning "no authored value in this cell" (water layer).
pub const NONE: u16 = u16::MAX;
/// Largest representable offset above a tile's base (exclusive of [`NONE`]).
pub const MAX_OFFSET_CM: i32 = NONE as i32 - 1;
/// Ground weight of an exact cell (the column's ground, no engine terrain).
pub const GROUND_EXACT: u8 = u8::MAX;
/// Top ground blocks a ground cell may hold: what terrain persistence (`i16`
/// z) and the probe's z range can carry. The exporter checks the tighter
/// height profile range.
pub const GROUND_BLOCK_RANGE: std::ops::RangeInclusive<i32> = -4096..=8191;

const MAGIC: &[u8; 4] = b"XART";
const VERSION: u16 = 1;
const HEADER_LEN: usize = 28;
/// zstd level the writers always use, so equal rasters give equal bytes.
#[cfg(any(test, feature = "tools"))]
const ZSTD_LEVEL: i32 = 19;
/// The first bytes of a Git LFS pointer file: what a checkout without the
/// LFS objects holds where a tile should be.
const LFS_POINTER_PREFIX: &[u8] = b"version https://git-lfs";

/// The raster layers this engine knows.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, serde::Deserialize, serde::Serialize)]
pub enum LayerKind {
    /// Stage 1: water surface + bed (or bank ground).
    Water,
    /// Stage 2: exact ground (`i32` cm) + blend weight per column.
    Ground,
}

impl LayerKind {
    /// The layer code stored in the tile header.
    pub fn code(self) -> u8 {
        match self {
            LayerKind::Water => 1,
            LayerKind::Ground => 2,
        }
    }

    /// The layer with this header code, if this engine knows it.
    pub fn from_code(code: u8) -> Option<Self> {
        match code {
            1 => Some(LayerKind::Water),
            2 => Some(LayerKind::Ground),
            _ => None,
        }
    }

    /// Decompressed payload size in bytes: the decoder knows it before it
    /// reads anything.
    pub fn payload_len(self) -> usize {
        match self {
            // surface u16, bed u16
            LayerKind::Water => TILE_CELLS * 2 * 2,
            // ground i32 (filtered), weight u8
            LayerKind::Ground => TILE_CELLS * 4 + TILE_CELLS,
        }
    }

    /// Name used in tile asset specifiers.
    pub fn asset_name(self) -> &'static str {
        match self {
            LayerKind::Water => "water",
            LayerKind::Ground => "ground",
        }
    }
}

/// Stored index of local cell `(i, j)` (both in `0..TILE_SIZE`).
#[inline(always)]
pub fn cell_index(i: i32, j: i32) -> usize { (j * TILE_SIZE + i) as usize }

/// A decoded water tile: two planes of stored values plus the tile's base.
pub struct WaterTile {
    pub origin: Vec2<i32>,
    pub base_cm: i32,
    pub surface: Box<[u16]>,
    pub bed: Box<[u16]>,
}

impl WaterTile {
    /// Stored index of local cell `(i, j)` (both in `0..TILE_SIZE`).
    #[inline(always)]
    pub fn idx(i: i32, j: i32) -> usize { cell_index(i, j) }

    #[inline(always)]
    fn value(&self, v: u16) -> Option<i32> { (v != NONE).then(|| self.base_cm + v as i32) }

    /// Authored surface (cm) at stored index `k`.
    #[inline(always)]
    pub fn surface_cm(&self, k: usize) -> Option<i32> { self.value(self.surface[k]) }

    /// Authored bed / bank ground (cm) at stored index `k`.
    #[inline(always)]
    pub fn bed_cm(&self, k: usize) -> Option<i32> { self.value(self.bed[k]) }

    /// Whether stored index `k` is a wet cell (surface and bed present).
    #[inline(always)]
    pub fn is_wet(&self, k: usize) -> bool { self.surface[k] != NONE && self.bed[k] != NONE }

    /// Whether stored index `k` holds any authored value.
    #[inline(always)]
    pub fn is_authored(&self, k: usize) -> bool { self.surface[k] != NONE || self.bed[k] != NONE }
}

/// A decoded ground tile in centimetres, as the loader validates it
/// (`ground[k]` is `None` exactly where `weight[k] == 0`).
pub struct GroundTileCm {
    pub origin: Vec2<i32>,
    pub ground: Box<[Option<i32>]>,
    pub weight: Box<[u8]>,
}

/// One resident ground cell: the top ground block (`i16`, little-endian) and
/// the blend weight, 3 bytes with alignment 1 (a lookup touches one cache
/// line).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[repr(C)]
pub struct GroundCell {
    block_le: [u8; 2],
    /// 0 = not authored; [`GROUND_EXACT`] = exact; else a blend weight.
    pub weight: u8,
}

impl GroundCell {
    /// Top ground block (meaningful when `weight > 0`).
    #[inline(always)]
    pub fn block(&self) -> i32 { i16::from_le_bytes(self.block_le) as i32 }
}

/// A resident ground tile: everything the engine uses after validation
/// (`floor(cm / 100)` and the weight), 192 KiB.
pub struct GroundTile {
    pub origin: Vec2<i32>,
    pub cells: Box<[GroundCell]>,
}

impl GroundTile {
    /// The authored ground at stored index `k`: `(top block, weight)`.
    #[inline(always)]
    pub fn get(&self, k: usize) -> Option<(i32, u8)> {
        let c = self.cells[k];
        (c.weight != 0).then(|| (c.block(), c.weight))
    }

    /// Pack a validated tile. Every present cell's block must lie in
    /// [`GROUND_BLOCK_RANGE`] (the loader checks it first); an error names
    /// the first cell that does not.
    pub fn from_cm(t: &GroundTileCm) -> Result<Self, String> {
        let mut cells = vec![GroundCell::default(); TILE_CELLS];
        for (k, (g, w)) in t.ground.iter().zip(t.weight.iter()).enumerate() {
            if let Some(cm) = g {
                let block = cm.div_euclid(100);
                if !GROUND_BLOCK_RANGE.contains(&block) {
                    return Err(format!(
                        "cell {k}: ground block {block} outside {GROUND_BLOCK_RANGE:?}"
                    ));
                }
                cells[k] = GroundCell {
                    block_le: (block as i16).to_le_bytes(),
                    weight: *w,
                };
            }
        }
        Ok(Self {
            origin: t.origin,
            cells: cells.into_boxed_slice(),
        })
    }
}

/// Bytes resident per decoded tile of `layer` (the memory budget).
pub const fn resident_tile_bytes(layer: LayerKind) -> usize {
    match layer {
        LayerKind::Water => TILE_CELLS * 2 * 2,
        LayerKind::Ground => TILE_CELLS * std::mem::size_of::<GroundCell>(),
    }
}

#[cfg(any(test, feature = "tools"))]
fn write_tile(
    layer: LayerKind,
    origin: Vec2<i32>,
    base_cm: i32,
    payload: &[u8],
) -> Result<Vec<u8>, String> {
    debug_assert_eq!(payload.len(), layer.payload_len());
    let compressed = zstd::bulk::compress(payload, ZSTD_LEVEL)
        .map_err(|e| format!("zstd compression failed: {e}"))?;
    let mut out = Vec::with_capacity(HEADER_LEN + compressed.len());
    out.extend_from_slice(MAGIC);
    out.extend_from_slice(&VERSION.to_le_bytes());
    out.push(layer.code());
    out.push(0);
    out.extend_from_slice(&(TILE_SIZE as u16).to_le_bytes());
    out.extend_from_slice(&0u16.to_le_bytes());
    out.extend_from_slice(&origin.x.to_le_bytes());
    out.extend_from_slice(&origin.y.to_le_bytes());
    out.extend_from_slice(&base_cm.to_le_bytes());
    out.extend_from_slice(&(compressed.len() as u32).to_le_bytes());
    out.extend_from_slice(&compressed);
    Ok(out)
}

/// Encode a water tile. `surface_cm` / `bed_cm` hold absolute centimetres
/// (`None` = not authored), `TILE_CELLS` each, rows from the south.
#[cfg(any(test, feature = "tools"))]
pub fn encode_water(
    origin: Vec2<i32>,
    surface_cm: &[Option<i32>],
    bed_cm: &[Option<i32>],
) -> Result<Vec<u8>, String> {
    if surface_cm.len() != TILE_CELLS || bed_cm.len() != TILE_CELLS {
        return Err(format!(
            "a tile has {TILE_CELLS} cells, got {} surface and {} bed values",
            surface_cm.len(),
            bed_cm.len()
        ));
    }
    let present = || surface_cm.iter().chain(bed_cm.iter()).filter_map(|v| *v);
    let base_cm = present().min().unwrap_or(0);
    let top = present().max().unwrap_or(0);
    if (top as i64) - (base_cm as i64) > MAX_OFFSET_CM as i64 {
        return Err(format!(
            "tile at {origin:?} spans {} cm of altitude; tile format 1 holds at most \
             {MAX_OFFSET_CM} cm per tile",
            top as i64 - base_cm as i64
        ));
    }
    let mut payload = Vec::with_capacity(LayerKind::Water.payload_len());
    for layer in [surface_cm, bed_cm] {
        for v in layer {
            let stored = match v {
                Some(v) => (v - base_cm) as u16,
                None => NONE,
            };
            payload.extend_from_slice(&stored.to_le_bytes());
        }
    }
    write_tile(LayerKind::Water, origin, base_cm, &payload)
}

/// Encode a ground tile: `ground_cm[k]` is the absolute ground in
/// centimetres with its weight (`None` = not authored), `TILE_CELLS` cells,
/// rows from the south. A present cell needs a weight of at least 1. The
/// filter (row-wise differences, byte shuffle) is described in the module
/// doc.
#[cfg(any(test, feature = "tools"))]
pub fn encode_ground(
    origin: Vec2<i32>,
    ground_cm: &[Option<(i32, u8)>],
) -> Result<Vec<u8>, String> {
    if ground_cm.len() != TILE_CELLS {
        return Err(format!(
            "a tile has {TILE_CELLS} cells, got {} ground values",
            ground_cm.len()
        ));
    }
    let mut deltas = vec![0u32; TILE_CELLS];
    let mut weights = vec![0u8; TILE_CELLS];
    for j in 0..TILE_SIZE {
        let mut prev = 0i32;
        for i in 0..TILE_SIZE {
            let k = cell_index(i, j);
            if let Some((cm, w)) = ground_cm[k] {
                if w == 0 {
                    return Err(format!(
                        "cell ({i}, {j}) of the tile at {origin:?}: ground {cm} cm with weight 0"
                    ));
                }
                deltas[k] = cm.wrapping_sub(prev) as u32;
                weights[k] = w;
                prev = cm;
            }
        }
    }
    let mut payload = Vec::with_capacity(LayerKind::Ground.payload_len());
    for byte in 0..4 {
        payload.extend(deltas.iter().map(|d| d.to_le_bytes()[byte]));
    }
    payload.extend_from_slice(&weights);
    write_tile(LayerKind::Ground, origin, 0, &payload)
}

/// Check the header and return the layer, the base and the decompressed
/// payload (its size checked against the layer).
fn read_tile(
    bytes: &[u8],
    expected_origin: Vec2<i32>,
    expected_layer: LayerKind,
) -> Result<(i32, Vec<u8>), String> {
    if bytes.starts_with(LFS_POINTER_PREFIX) {
        return Err(
            "the file is a Git LFS pointer, not the tile: fetch the LFS objects of this asset \
             root (`git lfs pull`)"
                .into(),
        );
    }
    if bytes.len() < HEADER_LEN {
        return Err(format!("{} bytes is shorter than the header", bytes.len()));
    }
    let u16_at = |o: usize| u16::from_le_bytes([bytes[o], bytes[o + 1]]);
    let i32_at =
        |o: usize| i32::from_le_bytes([bytes[o], bytes[o + 1], bytes[o + 2], bytes[o + 3]]);
    if &bytes[0..4] != MAGIC {
        return Err("bad magic (not an authored raster tile)".into());
    }
    let version = u16_at(4);
    if version != VERSION {
        return Err(format!(
            "unsupported tile format version {version} (this engine reads {VERSION})"
        ));
    }
    let layer = LayerKind::from_code(bytes[6]).ok_or_else(|| {
        format!(
            "unknown layer code {} (this engine knows: 1 = water, 2 = ground); the tile was \
             written by a newer exporter",
            bytes[6]
        )
    })?;
    if layer != expected_layer {
        return Err(format!(
            "layer {layer:?} where a {expected_layer:?} tile was expected"
        ));
    }
    if bytes[7] != 0 || u16_at(10) != 0 {
        return Err("reserved header bits are set".into());
    }
    if u16_at(8) as i32 != TILE_SIZE {
        return Err(format!("tile size {} (format 1 is {TILE_SIZE})", u16_at(8)));
    }
    let origin = Vec2::new(i32_at(12), i32_at(16));
    if origin != expected_origin {
        return Err(format!(
            "header origin {origin:?} does not match the manifest position {expected_origin:?}"
        ));
    }
    let base_cm = i32_at(20);
    let payload_len = u32::from_le_bytes([bytes[24], bytes[25], bytes[26], bytes[27]]) as usize;
    if payload_len != bytes.len() - HEADER_LEN {
        return Err(format!(
            "payload length {payload_len} does not match the {} bytes after the header",
            bytes.len() - HEADER_LEN
        ));
    }
    let want = layer.payload_len();
    let mut raw = Vec::with_capacity(want);
    // `take(want + 1)` bounds the decompressed size whatever the frame says.
    zstd::stream::read::Decoder::new(&bytes[HEADER_LEN..])
        .map_err(|e| format!("zstd: {e}"))?
        .take(want as u64 + 1)
        .read_to_end(&mut raw)
        .map_err(|e| format!("zstd: {e}"))?;
    if raw.len() != want {
        return Err(format!(
            "payload decompresses to {} bytes, expected {want}",
            raw.len()
        ));
    }
    Ok((base_cm, raw))
}

/// Decode and structurally validate a water tile whose
/// header must say `expected_origin`.
pub fn decode_water(bytes: &[u8], expected_origin: Vec2<i32>) -> Result<WaterTile, String> {
    let (base_cm, raw) = read_tile(bytes, expected_origin, LayerKind::Water)?;
    let (chunks, _) = raw.as_chunks::<2>();
    let values: Vec<u16> = chunks.iter().map(|c| u16::from_le_bytes(*c)).collect();
    let (surface, bed) = values.split_at(TILE_CELLS);
    let tile = WaterTile {
        origin: expected_origin,
        base_cm,
        surface: surface.into(),
        bed: bed.into(),
    };
    // `base + v` must stay in i32 for every stored value.
    let max_stored = tile
        .surface
        .iter()
        .chain(tile.bed.iter())
        .filter(|v| **v != NONE)
        .max()
        .copied();
    if let Some(v) = max_stored
        && base_cm.checked_add(v as i32).is_none()
    {
        return Err(format!("base {base_cm} cm + stored {v} overflows"));
    }
    Ok(tile)
}

/// Decode and structurally validate a ground tile whose header must say
/// `expected_origin`: base 0, every "not authored" cell (weight 0) stores
/// difference 0 (the canonical form).
pub fn decode_ground(bytes: &[u8], expected_origin: Vec2<i32>) -> Result<GroundTileCm, String> {
    let (base_cm, raw) = read_tile(bytes, expected_origin, LayerKind::Ground)?;
    if base_cm != 0 {
        return Err(format!("base {base_cm} (a ground tile's base must be 0)"));
    }
    let (filtered, weight) = raw.split_at(TILE_CELLS * 4);
    let mut ground = vec![None; TILE_CELLS];
    for j in 0..TILE_SIZE {
        let mut prev = 0i32;
        for i in 0..TILE_SIZE {
            let k = cell_index(i, j);
            let d = u32::from_le_bytes([
                filtered[k],
                filtered[TILE_CELLS + k],
                filtered[2 * TILE_CELLS + k],
                filtered[3 * TILE_CELLS + k],
            ]) as i32;
            if weight[k] == 0 {
                if d != 0 {
                    return Err(format!(
                        "cell ({i}, {j}): a ground value in a cell whose weight is 0 (not \
                         authored)"
                    ));
                }
            } else {
                prev = prev.wrapping_add(d);
                ground[k] = Some(prev);
            }
        }
    }
    Ok(GroundTileCm {
        origin: expected_origin,
        ground: ground.into_boxed_slice(),
        weight: weight.into(),
    })
}

/// Lower-case hex sha256 of `bytes` (the manifest's tile checksum).
pub fn sha256_hex(bytes: &[u8]) -> String {
    use sha2::Digest;
    let digest = sha2::Sha256::digest(bytes);
    digest.iter().map(|b| format!("{b:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> (Vec<Option<i32>>, Vec<Option<i32>>) {
        let mut s = vec![None; TILE_CELLS];
        let mut b = vec![None; TILE_CELLS];
        for k in 0..TILE_CELLS {
            if k % 7 == 0 {
                s[k] = Some(23_868);
                b[k] = Some(23_268 - (k % 300) as i32);
            } else if k % 5 == 0 {
                b[k] = Some(23_918);
            }
        }
        (s, b)
    }

    #[test]
    fn round_trip_is_exact_and_deterministic() {
        let (s, b) = sample();
        let o = Vec2::new(22_720, 24_544);
        let a = encode_water(o, &s, &b).unwrap();
        let a2 = encode_water(o, &s, &b).unwrap();
        assert_eq!(a, a2, "same raster, same bytes");
        let t = decode_water(&a, o).unwrap();
        for k in 0..TILE_CELLS {
            assert_eq!(t.surface_cm(k), s[k]);
            assert_eq!(t.bed_cm(k), b[k]);
        }
    }

    #[test]
    fn corrupt_tiles_are_errors_not_panics() {
        let (s, b) = sample();
        let o = Vec2::new(0, 0);
        let good = encode_water(o, &s, &b).unwrap();
        assert!(
            decode_water(&good, Vec2::new(256, 0)).is_err(),
            "origin mismatch"
        );
        assert!(decode_water(&good[..20], o).is_err(), "truncated header");
        assert!(
            decode_water(&good[..good.len() - 1], o).is_err(),
            "truncated payload"
        );
        let mut extra = good.clone();
        extra.push(0);
        assert!(decode_water(&extra, o).is_err(), "trailing byte");
        for at in [0usize, 4, 6, 7, 8, 10] {
            let mut bad = good.clone();
            bad[at] ^= 0x40;
            assert!(decode_water(&bad, o).is_err(), "flipped header byte {at}");
        }
        let mut bad = good.clone();
        let mid = HEADER_LEN + (good.len() - HEADER_LEN) / 2;
        bad[mid] ^= 0xFF;
        // A flipped payload byte either fails zstd's checks or changes the
        // data; the manifest sha256 is what catches the second case.
        if decode_water(&bad, o).is_ok() {
            assert_ne!(sha256_hex(&bad), sha256_hex(&good));
        }
    }

    #[test]
    fn unknown_layer_codes_and_lfs_pointers_have_clear_errors() {
        let (s, b) = sample();
        let o = Vec2::new(0, 0);
        let mut t = encode_water(o, &s, &b).unwrap();
        t[6] = 3;
        let e = decode_water(&t, o).err().unwrap();
        assert!(e.contains("unknown layer code 3"), "{e}");
        t[6] = LayerKind::Ground.code();
        let e = decode_water(&t, o).err().unwrap();
        assert!(e.contains("layer Ground where a Water tile"), "{e}");
        let pointer = b"version https://git-lfs.github.com/spec/v1\noid sha256:00\nsize 1\n";
        let e = decode_water(pointer, o).err().unwrap();
        assert!(e.contains("Git LFS pointer"), "{e}");
    }

    #[test]
    fn a_tile_spanning_more_than_u16_centimetres_is_refused() {
        let mut s = vec![None; TILE_CELLS];
        let mut b = vec![None; TILE_CELLS];
        s[0] = Some(80_000);
        b[0] = Some(10_000);
        assert!(encode_water(Vec2::zero(), &s, &b).is_err());
        b[0] = Some(80_000 - MAX_OFFSET_CM);
        assert!(encode_water(Vec2::zero(), &s, &b).is_ok());
    }

    /// A ramp, a cliff, flats, holes and extreme values: what the filter has
    /// to carry exactly.
    fn ground_sample() -> Vec<Option<(i32, u8)>> {
        let mut g = vec![None; TILE_CELLS];
        for j in 0..TILE_SIZE {
            for i in 0..TILE_SIZE {
                let k = cell_index(i, j);
                g[k] = match (i, j) {
                    // not authored
                    (_, 0..=9) => None,
                    (0..=15, _) => None,
                    // a 1:20 ramp in x
                    (16..=99, _) => Some((23_918 + i * 5, GROUND_EXACT)),
                    // a 90 m cliff
                    (100..=119, _) => Some((500_000, GROUND_EXACT)),
                    (120..=139, _) => Some((491_000, GROUND_EXACT)),
                    // the bottom and top of the block range
                    (140, _) => Some((-409_600, GROUND_EXACT)),
                    (141, _) => Some((819_199, GROUND_EXACT)),
                    // a hole in the middle of a row
                    (142..=149, _) => None,
                    // blend weights
                    (150..=160, _) => Some((-1_234, (i - 149) as u8)),
                    _ => Some((14_050 + j, GROUND_EXACT)),
                };
            }
        }
        g
    }

    #[test]
    fn ground_round_trip_is_exact_and_deterministic() {
        let g = ground_sample();
        let o = Vec2::new(22_720, 24_544);
        let a = encode_ground(o, &g).unwrap();
        assert_eq!(a, encode_ground(o, &g).unwrap(), "same raster, same bytes");
        assert_eq!(a[6], 2, "layer code 2");
        assert_eq!(&a[20..24], &[0, 0, 0, 0], "base 0");
        let t = decode_ground(&a, o).unwrap();
        for (k, cell) in g.iter().enumerate() {
            assert_eq!(t.ground[k], cell.map(|(cm, _)| cm), "cell {k}");
            assert_eq!(t.weight[k], cell.map_or(0, |(_, w)| w), "cell {k}");
        }
        let packed = GroundTile::from_cm(&t).unwrap();
        assert_eq!(std::mem::size_of::<GroundCell>(), 3);
        assert_eq!(std::mem::align_of::<GroundCell>(), 1);
        assert_eq!(resident_tile_bytes(LayerKind::Ground), 192 << 10);
        for (k, cell) in g.iter().enumerate() {
            assert_eq!(
                packed.get(k),
                cell.map(|(cm, w)| (cm.div_euclid(100), w)),
                "cell {k}"
            );
        }
        // Wrong layer, wrong origin.
        assert!(decode_water(&a, o).is_err());
        assert!(decode_ground(&a, o + 256).is_err());
    }

    /// The filter is what makes slopes cheap: a whole tile of a 1:20 ramp
    /// compresses to a few hundred bytes, a real-looking relief stays small.
    #[test]
    fn ground_filter_compresses_slopes() {
        let o = Vec2::zero();
        let ramp: Vec<Option<(i32, u8)>> = (0..TILE_CELLS)
            .map(|k| Some((20_000 + (k as i32 % TILE_SIZE) * 5, GROUND_EXACT)))
            .collect();
        let flat: Vec<Option<(i32, u8)>> = vec![Some((23_918, GROUND_EXACT)); TILE_CELLS];
        let relief: Vec<Option<(i32, u8)>> = (0..TILE_CELLS)
            .map(|k| {
                let (x, y) = ((k as i32 % TILE_SIZE) as f32, (k as i32 / TILE_SIZE) as f32);
                let cm = 300_000.0
                    + 4_000.0 * (x / 23.0).sin() * (y / 31.0).cos()
                    + 900.0 * (x / 7.0 + y / 11.0).sin();
                Some((cm.round() as i32, GROUND_EXACT))
            })
            .collect();
        let (r, f, m) = (
            encode_ground(o, &ramp).unwrap().len(),
            encode_ground(o, &flat).unwrap().len(),
            encode_ground(o, &relief).unwrap().len(),
        );
        println!("ground tile sizes: ramp {r} B, flat {f} B, relief {m} B");
        assert!(r < 2_048, "ramp tile {r} bytes");
        assert!(f < 1_024, "flat tile {f} bytes");
        assert!(m < 160_000, "relief tile {m} bytes");
    }

    #[test]
    fn non_canonical_and_corrupt_ground_tiles_are_errors() {
        let g = ground_sample();
        let o = Vec2::new(0, 0);
        let good = encode_ground(o, &g).unwrap();
        // A value in a weight-0 cell: rebuild the payload by hand.
        let raw = {
            let (_, raw) = read_tile(&good, o, LayerKind::Ground).unwrap();
            raw
        };
        let mut bad = raw.clone();
        bad[0] = 7; // cell (0, 0) has weight 0
        let tile = write_tile(LayerKind::Ground, o, 0, &bad).unwrap();
        let e = decode_ground(&tile, o).err().unwrap();
        assert!(e.contains("weight is 0"), "{e}");
        // A non-zero base.
        let tile = write_tile(LayerKind::Ground, o, 5, &raw).unwrap();
        let e = decode_ground(&tile, o).err().unwrap();
        assert!(e.contains("base must be 0"), "{e}");
        // Truncations and flipped header bytes.
        assert!(decode_ground(&good[..good.len() - 1], o).is_err());
        for at in [0usize, 4, 6, 7, 8, 10] {
            let mut bad = good.clone();
            bad[at] ^= 0x40;
            assert!(decode_ground(&bad, o).is_err(), "flipped header byte {at}");
        }
        // A present cell needs a weight.
        let mut g0 = vec![None; TILE_CELLS];
        g0[3] = Some((100, 0));
        assert!(encode_ground(o, &g0).is_err());
        // Packing refuses a block outside the range.
        let mut g1 = vec![None; TILE_CELLS];
        g1[3] = Some((819_200, GROUND_EXACT));
        let t = decode_ground(&encode_ground(o, &g1).unwrap(), o).unwrap();
        let e = GroundTile::from_cm(&t).err().unwrap();
        assert!(e.contains("outside"), "{e}");
        g1[3] = Some((-409_601, GROUND_EXACT));
        let t = decode_ground(&encode_ground(o, &g1).unwrap(), o).unwrap();
        assert!(GroundTile::from_cm(&t).is_err());
    }
}
