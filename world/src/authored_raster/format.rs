//! Binary tile codec of the authored raster layers (format v1).
//!
//! One tile is 256 x 256 cells of 1 m, one cell per block column, rows from
//! south to north, `x` fastest. Layout (little-endian):
//!
//! | offset | field | value |
//! |---:|---|---|
//! | 0 | magic | `b"XART"` |
//! | 4 | version | `u16` = 1 |
//! | 6 | layer | `u8` (1 = water) |
//! | 7 | flags | `u8` = 0 |
//! | 8 | tile size | `u16` = 256 |
//! | 10 | reserved | `u16` = 0 |
//! | 12 | origin | `i32` x, `i32` y (wpos of cell 0,0) |
//! | 20 | base | `i32` centimetres |
//! | 24 | payload length | `u32` |
//! | 28 | payload | zstd of `surface[u16; N]` then `bed[u16; N]` |
//!
//! A stored value of [`NONE`] means "not authored"; any other value `v`
//! means the altitude `base + v` centimetres in the engine's block-z frame.
//!
//! Decoding never trusts a length it read: the payload length must equal the
//! rest of the file and the decompressed size is fixed by the format, so a
//! corrupt tile is an error, never an oversized allocation.

use std::io::Read;
use vek::Vec2;

/// Edge length of a tile in cells (= metres = block columns).
pub const TILE_SIZE: i32 = 256;
/// Cells per tile.
pub const TILE_CELLS: usize = (TILE_SIZE * TILE_SIZE) as usize;
/// Stored value meaning "no authored value in this cell".
pub const NONE: u16 = u16::MAX;
/// Largest representable offset above a tile's base (exclusive of [`NONE`]).
pub const MAX_OFFSET_CM: i32 = NONE as i32 - 1;

const MAGIC: &[u8; 4] = b"XART";
const VERSION: u16 = 1;
const HEADER_LEN: usize = 28;
/// zstd level the writer always uses, so equal rasters give equal bytes.
const ZSTD_LEVEL: i32 = 19;

/// The raster layers format v1 knows.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, serde::Deserialize, serde::Serialize)]
pub enum LayerKind {
    /// Stage 1: water surface + bed (or bank ground).
    Water,
}

impl LayerKind {
    fn code(self) -> u8 {
        match self {
            LayerKind::Water => 1,
        }
    }

    /// Name used in tile asset specifiers.
    pub fn asset_name(self) -> &'static str {
        match self {
            LayerKind::Water => "water",
        }
    }
}

/// A decoded water tile: two layers of stored values plus the tile's base.
pub struct RawTile {
    pub origin: Vec2<i32>,
    pub base_cm: i32,
    pub surface: Box<[u16]>,
    pub bed: Box<[u16]>,
}

impl RawTile {
    /// Stored index of local cell `(i, j)` (both in `0..TILE_SIZE`).
    #[inline(always)]
    pub fn idx(i: i32, j: i32) -> usize { (j * TILE_SIZE + i) as usize }

    #[inline(always)]
    fn value(&self, v: u16) -> Option<i32> { (v != NONE).then(|| self.base_cm + v as i32) }

    /// Authored surface (cm) at stored index `k`.
    #[inline(always)]
    pub fn surface_cm(&self, k: usize) -> Option<i32> { self.value(self.surface[k]) }

    /// Authored bed / bank ground (cm) at stored index `k`.
    #[inline(always)]
    pub fn bed_cm(&self, k: usize) -> Option<i32> { self.value(self.bed[k]) }
}

/// Encode a water tile. `surface_cm` / `bed_cm` hold absolute centimetres
/// (`None` = not authored), `TILE_CELLS` each, rows from the south.
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
            "tile at {origin:?} spans {} cm of altitude; format v1 holds at most {MAX_OFFSET_CM} \
             cm per tile",
            top as i64 - base_cm as i64
        ));
    }
    let mut payload = Vec::with_capacity(TILE_CELLS * 4);
    for layer in [surface_cm, bed_cm] {
        for v in layer {
            let stored = match v {
                Some(v) => (v - base_cm) as u16,
                None => NONE,
            };
            payload.extend_from_slice(&stored.to_le_bytes());
        }
    }
    let compressed = zstd::bulk::compress(&payload, ZSTD_LEVEL)
        .map_err(|e| format!("zstd compression failed: {e}"))?;
    let mut out = Vec::with_capacity(HEADER_LEN + compressed.len());
    out.extend_from_slice(MAGIC);
    out.extend_from_slice(&VERSION.to_le_bytes());
    out.push(LayerKind::Water.code());
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

/// Decode and structurally validate a water tile whose header must say
/// `expected_origin`.
pub fn decode_water(bytes: &[u8], expected_origin: Vec2<i32>) -> Result<RawTile, String> {
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
            "unsupported tile version {version} (this engine reads {VERSION})"
        ));
    }
    if bytes[6] != LayerKind::Water.code() {
        return Err(format!("layer code {} is not water", bytes[6]));
    }
    if bytes[7] != 0 || u16_at(10) != 0 {
        return Err("reserved header bits are set".into());
    }
    if u16_at(8) as i32 != TILE_SIZE {
        return Err(format!(
            "tile size {} (format v1 is {TILE_SIZE})",
            u16_at(8)
        ));
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
    let want = TILE_CELLS * 4;
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
    let (chunks, _) = raw.as_chunks::<2>();
    let values: Vec<u16> = chunks.iter().map(|c| u16::from_le_bytes(*c)).collect();
    let (surface, bed) = values.split_at(TILE_CELLS);
    let tile = RawTile {
        origin,
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
        if let Ok(t) = decode_water(&bad, o) {
            assert_ne!(sha256_hex(&bad), sha256_hex(&good));
            let _ = t;
        }
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
}
