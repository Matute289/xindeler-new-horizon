//! Edge-case battery for the `tprobe v1` format (reader/writer and
//! `summarize`). Pure: no engine, no assets. Ids `EC-D*` refer to the
//! precision-tooling corner-case catalogue; tests that expose a bug in the
//! current code are `#[ignore = "BUG-.."]` so the suite stays green while the
//! bug is tracked (run them with `--ignored` to see them fail).

use std::{
    collections::BTreeMap,
    panic::{AssertUnwindSafe, catch_unwind},
};

use super::*;

/// Tiny deterministic PRNG (xorshift64*), so fuzzers need no extra crate.
pub(crate) struct Rng(u64);

impl Rng {
    pub(crate) fn new(seed: u64) -> Self { Self(seed.max(1)) }

    pub(crate) fn next(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.0 = x;
        x.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }

    pub(crate) fn below(&mut self, n: u64) -> u64 { self.next() % n.max(1) }
}

pub(crate) fn header(box_xy: [i32; 4], zmin: i32, zmax: i32) -> Header {
    Header {
        format: FORMAT_NAME.into(),
        box_xy,
        zmin,
        zmax,
        nx: (box_xy[2] - box_xy[0]).max(0) as u32,
        ny: (box_xy[3] - box_xy[1]).max(0) as u32,
        seed: 0,
        path: "fast".into(),
        calendar: None,
        engine_commit: "test".into(),
        assets: BTreeMap::new(),
        class_codes: Header::class_codes(),
        block_kinds: BTreeMap::new(),
        stats: BTreeMap::new(),
        sections: vec![],
    }
}

/// A dump whose columns are given as class stacks (bottom to top).
pub(crate) fn dump_from_columns(box_xy: [i32; 4], zmin: i32, cols: &[Vec<u8>]) -> Dump {
    let h = cols.first().map_or(0, Vec::len) as i32;
    let mut d = Dump {
        header: header(box_xy, zmin, zmin + h),
        alt: vec![],
        riverless_alt: vec![],
        water_level: vec![],
        warp_factor: vec![],
        top_kind: vec![],
        flags: vec![],
        ground_top: vec![],
        water_top: vec![],
        liquid_depth: vec![],
        run_counts: vec![],
        run_class: vec![],
        run_len: vec![],
        sim_csv: String::new(),
        sites: vec![],
    };
    for c in cols {
        let runs = rle(c.iter().copied());
        let s = summarize(zmin, &runs);
        d.alt.push(f32::from(s.ground_top) + 0.5);
        d.riverless_alt.push(f32::from(s.ground_top) + 0.5);
        d.water_level.push(f32::NAN);
        d.warp_factor.push(0.0);
        d.top_kind.push(if s.ground_top == NO_Z { 255 } else { 3 });
        d.flags.push(s.flags);
        d.ground_top.push(s.ground_top);
        d.water_top.push(s.water_top);
        d.liquid_depth.push(s.liquid_depth);
        d.run_counts.push(runs.len() as u16);
        for (cl, n) in runs {
            d.run_class.push(cl);
            d.run_len.push(n);
        }
    }
    d
}

pub(crate) fn random_dump(rng: &mut Rng, nx: i32, ny: i32, h: usize) -> Dump {
    let cols: Vec<Vec<u8>> = (0..nx * ny)
        .map(|_| {
            let g = rng.below(h as u64) as usize;
            let w = g + rng.below((h - g) as u64 + 1) as usize;
            (0..h)
                .map(|z| {
                    if z < g {
                        if rng.below(20) == 0 {
                            class::AIR
                        } else {
                            class::GROUND
                        }
                    } else if z < w {
                        class::LIQUID
                    } else if rng.below(30) == 0 {
                        class::STRUCTURE
                    } else {
                        class::AIR
                    }
                })
                .collect()
        })
        .collect();
    dump_from_columns([1000, 2000, 1000 + nx, 2000 + ny], 100, &cols)
}

pub(crate) fn bytes(d: &Dump) -> Vec<u8> {
    let mut v = Vec::new();
    d.write_to(&mut v).unwrap();
    v
}

/// Header JSON offset/length of a serialised dump.
fn header_span(b: &[u8]) -> (usize, usize) {
    (
        12,
        u32::from_le_bytes(b[8..12].try_into().unwrap()) as usize,
    )
}

/// Re-serialise a file with a modified header JSON (sections untouched).
fn with_header(b: &[u8], edit: impl FnOnce(&mut serde_json::Value)) -> Vec<u8> {
    let (o, l) = header_span(b);
    let mut h: serde_json::Value = serde_json::from_slice(&b[o..o + l]).unwrap();
    edit(&mut h);
    let hj = serde_json::to_vec(&h).unwrap();
    let mut out = Vec::new();
    out.extend_from_slice(MAGIC);
    out.extend_from_slice(&(hj.len() as u32).to_le_bytes());
    out.extend_from_slice(&hj);
    out.extend_from_slice(&b[o + l..]);
    out
}

/// `Ok(true)` = parsed, `Ok(false)` = clean error, `Err(msg)` = panicked.
pub(crate) fn read_outcome(b: &[u8]) -> Result<bool, String> {
    match catch_unwind(AssertUnwindSafe(|| Dump::read_from(b).is_ok())) {
        Ok(ok) => Ok(ok),
        Err(p) => Err(p
            .downcast_ref::<String>()
            .cloned()
            .or_else(|| p.downcast_ref::<&str>().map(|s| (*s).to_string()))
            .unwrap_or_default()),
    }
}

// ------------------------------------------------------------- determinism

/// EC-D20: same dump -> same bytes, for many random dumps; read->write is a
/// fixed point; `class_at` agrees with the runs at every z incl. both ends.
#[test]
fn ec_d20_random_dumps_round_trip_and_are_byte_stable() {
    let mut rng = Rng::new(0xD20);
    for _ in 0..40 {
        let nx = 1 + rng.below(9) as i32;
        let ny = 1 + rng.below(9) as i32;
        let h = 1 + rng.below(60) as usize;
        let d = random_dump(&mut rng, nx, ny, h);
        let b = bytes(&d);
        assert_eq!(b, bytes(&d));
        let r = Dump::read_from(&b[..]).unwrap();
        assert_eq!(bytes(&r), b);
        let off = r.run_offsets();
        for idx in 0..r.header.columns() {
            let mut z = r.header.zmin;
            for (c, n) in r.runs_at(&off, idx) {
                for k in 0..i32::from(n) {
                    assert_eq!(r.class_at(&off, idx, z + k), Some(c));
                }
                z += i32::from(n);
            }
            assert_eq!(r.class_at(&off, idx, r.header.zmin - 1), None);
            assert_eq!(r.class_at(&off, idx, r.header.zmax), None);
        }
    }
}

/// EC-D21: the 1x1 box and a 1-block z range are valid dumps.
#[test]
fn ec_d21_one_column_one_block() {
    for c in [class::AIR, class::GROUND, class::LIQUID, class::STRUCTURE] {
        let d = dump_from_columns([5, 5, 6, 6], -3, &[vec![c]]);
        let r = Dump::read_from(&bytes(&d)[..]).unwrap();
        assert_eq!(r.header.columns(), 1);
        assert_eq!(r.class_at(&r.run_offsets(), 0, -3), Some(c));
        let clipped = r.flags[0] & flag::CLIPPED_TOP != 0;
        assert_eq!(
            clipped,
            c != class::AIR,
            "a non-air top block means clipped"
        );
    }
}

/// EC-D22: an empty box (0 columns) round-trips (writer must not panic).
#[test]
fn ec_d22_empty_box_round_trips() {
    let d = dump_from_columns([7, 7, 7, 7], 0, &[]);
    let r = Dump::read_from(&bytes(&d)[..]).unwrap();
    assert_eq!(r.header.columns(), 0);
}

/// EC-D23: a column taller than u16::MAX blocks splits into several runs of
/// the same class and is still summarised correctly.
#[test]
fn ec_d23_runs_longer_than_u16_split() {
    let h = 70_000usize;
    let mut col = vec![class::GROUND; h];
    col[h - 1] = class::AIR;
    let runs = rle(col.iter().copied());
    assert_eq!(runs.len(), 3, "65535 + 4464 ground + 1 air");
    assert_eq!(runs.iter().map(|&(_, n)| n as usize).sum::<usize>(), h);
    let s = summarize(-40_000, &runs);
    assert_eq!(i32::from(s.ground_top), -40_000 + h as i32 - 2);
}

/// EC-D24: the NO_Z sentinel collides with a real z of -32768: a column whose
/// only ground block sits at z = -32768 reports "no ground".
#[test]
#[ignore = "BUG-P3: ground_top at z=-32768 is indistinguishable from NO_Z (sentinel collision)"]
fn ec_d24_sentinel_collision_at_i16_min() {
    let s = summarize(-32768, &[(class::GROUND, 1), (class::AIR, 9)]);
    assert_ne!(
        s.ground_top, NO_Z,
        "a real ground block at -32768 must not read as none"
    );
}

/// EC-D25: `summarize` silently wraps z outside the i16 range (the dump
/// command only bounds zmax - zmin, not zmin/zmax themselves).
#[test]
#[ignore = "BUG-P3: ground_top/water_top are cast to i16 without a range check (z beyond +-32767 \
            wraps)"]
fn ec_d25_z_outside_i16_wraps() {
    let s = summarize(-40_000, &[(class::GROUND, 1000), (class::AIR, 10)]);
    assert_eq!(i32::from(s.ground_top), -39_001);
    let s = summarize(40_000, &[(class::GROUND, 5), (class::AIR, 10)]);
    assert_eq!(i32::from(s.ground_top), 40_004);
}

/// EC-D26: semantic cases of `summarize` that verifiers rely on.
#[test]
fn ec_d26_summary_semantics() {
    use class::*;
    // Water on a roof (structure) above the ground: liquid counted above
    // ground, structure flag, not a void.
    let s = summarize(0, &rle([GROUND, GROUND, AIR, STRUCTURE, LIQUID, AIR]));
    assert_eq!((s.ground_top, s.water_top, s.liquid_depth), (1, 4, 1));
    assert_eq!(s.flags & flag::VOID, 0);
    assert_ne!(s.flags & flag::STRUCTURE, 0);
    // Water in a cave under ground: void, liquid flag, zero depth above ground.
    let s = summarize(0, &rle([GROUND, LIQUID, AIR, GROUND, GROUND, AIR]));
    assert_ne!(s.flags & flag::VOID, 0);
    assert_ne!(s.flags & flag::LIQUID, 0);
    assert_eq!(s.liquid_depth, 0);
    assert_eq!(
        s.water_top, 1,
        "water_top is the topmost liquid even if underground"
    );
    // Overhang / natural arch: ground above air above ground is a VOID column.
    let s = summarize(0, &rle([GROUND, AIR, AIR, AIR, GROUND, AIR]));
    assert_ne!(s.flags & flag::VOID, 0);
    assert_eq!(s.ground_top, 4);
    // All air: no ground, no flags.
    let s = summarize(50, &rle([AIR; 5]));
    assert_eq!((s.ground_top, s.flags), (NO_Z, 0));
    // All solid: clipped top.
    let s = summarize(0, &rle([GROUND; 5]));
    assert_eq!(s.ground_top, 4);
    assert_ne!(s.flags & flag::CLIPPED_TOP, 0);
    // Unloaded on top is not "clipped"; unloaded below ground is not a void.
    let s = summarize(0, &rle([GROUND, UNLOADED, GROUND, UNLOADED]));
    assert_eq!(s.flags & (flag::CLIPPED_TOP | flag::VOID), 0);
    // Sprite above ground is not clipped; a sprite under ground is a void.
    let s = summarize(0, &rle([GROUND, SPRITE, GROUND, SPRITE]));
    assert_ne!(s.flags & flag::VOID, 0);
    assert_eq!(s.flags & flag::CLIPPED_TOP, 0);
    // Zero-height column (zmin == zmax): nothing.
    let s = summarize(0, &[]);
    assert_eq!((s.ground_top, s.water_top, s.flags), (NO_Z, NO_Z, 0));
}

/// EC-D27: a column cut from below by `--zmin` (all air because the terrain
/// is lower than zmin) carries no flag: the format cannot tell it from a
/// shaft that continues below zmin. Characterisation test of the gap.
#[test]
fn ec_d27_no_clipped_bottom_flag_exists() {
    let shaft = summarize(0, &rle([class::AIR; 4]));
    let above_terrain = summarize(1000, &rle([class::AIR; 4]));
    assert_eq!(shaft, above_terrain);
}

// ------------------------------------------------------------- corruption

/// EC-D01: every truncation of a valid file is a clean error, never a panic.
#[test]
fn ec_d01_every_truncation_is_a_clean_error() {
    let d = random_dump(&mut Rng::new(1), 3, 2, 12);
    let b = bytes(&d);
    for n in 0..b.len() {
        assert_eq!(
            read_outcome(&b[..n]),
            Ok(false),
            "truncated at {n}/{}",
            b.len()
        );
    }
}

/// EC-D02: trailing bytes after the last section are accepted silently (a
/// concatenated or partially overwritten file reads as valid).
#[test]
#[ignore = "BUG-P4: trailing garbage after the last section is ignored by Dump::read_from"]
fn ec_d02_trailing_garbage_is_rejected() {
    let mut b = bytes(&random_dump(&mut Rng::new(2), 2, 2, 8));
    b.extend_from_slice(b"junk");
    assert_eq!(read_outcome(&b), Ok(false));
}

/// EC-D03: single-bit flips anywhere in the file never panic (they may parse
/// when they hit an f32 payload, that is fine).
#[test]
fn ec_d03_bit_flips_never_panic() {
    let b = bytes(&random_dump(&mut Rng::new(3), 3, 3, 10));
    let mut panics = Vec::new();
    for pos in 0..b.len() {
        for bit in 0..8 {
            let mut c = b.clone();
            c[pos] ^= 1 << bit;
            if let Err(m) = read_outcome(&c) {
                panics.push((pos, bit, m));
            }
        }
    }
    assert!(
        panics.is_empty(),
        "{} panics, first {:?}",
        panics.len(),
        panics.first()
    );
}

/// EC-D04: a huge `comp_len`/`raw_len` in the section table must be an error,
/// not an allocation panic/abort.
#[test]
#[ignore = "BUG-P1: comp_len/raw_len from the header are allocated before any bound check \
            (capacity overflow panic)"]
fn ec_d04_huge_section_lengths_are_errors() {
    let b = bytes(&random_dump(&mut Rng::new(4), 2, 2, 6));
    for field in ["comp_len", "raw_len"] {
        let c = with_header(&b, |h| {
            h["sections"][0][field] = serde_json::json!(u64::MAX);
        });
        assert_eq!(read_outcome(&c), Ok(false), "{field} = u64::MAX");
    }
}

/// EC-D05: a header whose nx/ny disagree with box_xy (same column count) is
/// accepted, and `col_index` then addresses the wrong column.
#[test]
#[ignore = "BUG-P2: Dump::validate does not check nx == x1-x0 and ny == y1-y0 (wrong columns / \
            panic in runs_at)"]
fn ec_d05_nx_ny_must_match_the_box() {
    let d = random_dump(&mut Rng::new(5), 4, 1, 6);
    let b = bytes(&d);
    // 4x1 box, header claims 1x4: same number of columns.
    let c = with_header(&b, |h| {
        h["nx"] = serde_json::json!(1);
        h["ny"] = serde_json::json!(4);
    });
    assert_eq!(read_outcome(&c), Ok(false));
}

/// EC-D06: zmax < zmin with no columns passes validation (height() wraps).
#[test]
#[ignore = "BUG-P4: an inverted z range (zmax < zmin) is not rejected by the reader"]
fn ec_d06_inverted_z_range_is_rejected() {
    let d = dump_from_columns([0, 0, 0, 0], 10, &[]);
    let b = with_header(&bytes(&d), |h| {
        h["zmin"] = serde_json::json!(10);
        h["zmax"] = serde_json::json!(5);
    });
    assert_eq!(read_outcome(&b), Ok(false));
}

/// EC-D07: unknown class codes inside run_class are accepted.
#[test]
#[ignore = "BUG-P4: run_class values outside the declared class_codes are not rejected"]
fn ec_d07_unknown_class_codes_are_rejected() {
    let mut d = random_dump(&mut Rng::new(7), 2, 1, 4);
    d.run_class[0] = 77;
    assert!(d.validate().is_err());
}

/// EC-D08: a different format name / magic is refused (version mismatch);
/// unknown extra header keys are tolerated (forward compatibility).
#[test]
fn ec_d08_version_mismatch_is_refused() {
    let b = bytes(&random_dump(&mut Rng::new(8), 2, 2, 4));
    let c = with_header(&b, |h| h["format"] = serde_json::json!("tprobe v2"));
    assert_eq!(read_outcome(&c), Ok(false));
    let mut m = b.clone();
    m[6] = b'2'; // "TPROBE2\n"
    assert_eq!(read_outcome(&m), Ok(false));
    let c = with_header(&b, |h| h["future_key"] = serde_json::json!(1));
    assert_eq!(read_outcome(&c), Ok(true));
}

/// EC-D09: a zstd section corrupted in the middle never panics.
#[test]
fn ec_d09_zstd_payload_corruption_never_panics() {
    let d = random_dump(&mut Rng::new(9), 6, 6, 40);
    let b = bytes(&d);
    let (o, l) = header_span(&b);
    let start = o + l;
    let mut rng = Rng::new(99);
    for _ in 0..300 {
        let mut c = b.clone();
        let p = start + rng.below((b.len() - start) as u64) as usize;
        c[p] = c[p].wrapping_add(1 + rng.below(255) as u8);
        assert!(read_outcome(&c).is_ok(), "panic with byte {p} changed");
    }
}

/// EC-D10: non-finite site radii do not survive a round trip (serde_json
/// writes NaN/inf as null).
#[test]
#[ignore = "BUG-P4: SiteRec.radius NaN/inf serialises as null and reads back as None (lossy round \
            trip)"]
fn ec_d10_site_radius_non_finite_round_trip() {
    let mut d = random_dump(&mut Rng::new(10), 1, 1, 3);
    d.sites.push(SiteRec {
        source: "world_site".into(),
        id: None,
        name: None,
        kind: None,
        wx: 0,
        wy: 0,
        radius: Some(f32::INFINITY),
    });
    let r = Dump::read_from(&bytes(&d)[..]).unwrap();
    assert_eq!(r.sites[0].radius, Some(f32::INFINITY));
}

/// EC-D11: f32 bit patterns (NaN payloads, -0.0, subnormals, inf) survive the
/// byte shuffle exactly.
#[test]
fn ec_d11_float_bit_patterns_survive() {
    let mut d = random_dump(&mut Rng::new(11), 3, 2, 3);
    let pats = [
        f32::from_bits(0x7fc0_0001),
        -0.0,
        f32::from_bits(1),
        f32::INFINITY,
        f32::NEG_INFINITY,
        f32::MAX,
    ];
    d.alt.copy_from_slice(&pats);
    let r = Dump::read_from(&bytes(&d)[..]).unwrap();
    let bits = |v: &[f32]| v.iter().map(|x| x.to_bits()).collect::<Vec<_>>();
    assert_eq!(bits(&r.alt), bits(&pats));
}

/// EC-D12: endianness is explicit: multi-byte values are little-endian on disk
/// regardless of the host (the shuffle puts all low bytes first).
#[test]
fn ec_d12_little_endian_on_disk() {
    assert_eq!(pack_u16(&[258]), vec![0x02, 0x01]);
    assert_eq!(pack_u16(&[258, 3]), vec![0x02, 0x03, 0x01, 0x00]);
    assert_eq!(pack_f32(&[1.0]), 1.0f32.to_le_bytes().to_vec());
}
