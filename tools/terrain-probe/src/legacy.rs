//! Comparison against the dense dump layout used by the early scratch
//! harness: `blocks.bin` (one class byte per block, index
//! `(ix * ny + iy) * h + (z - zmin)`, classes 0 air / 1 ground / 2 liquid /
//! 3 unloaded / 4 other solid, incl. solid sprites), `cols.f32` (alt,
//! riverless_alt, water_level, warp_factor per column, same column order) and
//! `meta.txt` (`x0 y0 x1 y1 1 zmin zmax nx ny`).

use std::path::Path;

use crate::{
    format::{Dump, class},
    probe::Res,
};

/// Returns true when every compared class and column float is identical.
pub fn compare(d: &Dump, dir: &Path) -> Res<bool> {
    let meta = std::fs::read_to_string(dir.join("meta.txt"))?;
    let v: Vec<i32> = meta
        .split_whitespace()
        .map(str::parse)
        .collect::<Result<_, _>>()?;
    let [x0, y0, x1, y1, _, zmin, zmax, nx, ny] = v[..] else {
        return Err("meta.txt: expected 9 integers".into());
    };
    let h = &d.header;
    if h.box_xy != [x0, y0, x1, y1] || h.zmin != zmin || h.zmax != zmax {
        return Err(format!(
            "dump covers {:?} z {}..{}, legacy covers {:?} z {zmin}..{zmax}",
            h.box_xy,
            h.zmin,
            h.zmax,
            [x0, y0, x1, y1]
        )
        .into());
    }
    let blocks = std::fs::read(dir.join("blocks.bin"))?;
    let cols = std::fs::read(dir.join("cols.f32"))?;
    let hh = (zmax - zmin) as usize;
    let (nx, ny) = (nx as usize, ny as usize);
    if blocks.len() != nx * ny * hh || cols.len() != nx * ny * 16 {
        return Err("legacy file sizes do not match meta.txt".into());
    }
    let off = d.run_offsets();
    let mut class_diff = 0u64;
    let mut sprite_noise_cells = 0u64;
    let mut unloaded_cells = 0u64;
    let mut col_diff = [0u64; 4];
    let mut first: Option<(i32, i32, i32, u8, u8)> = None;
    for ix in 0..nx {
        for iy in 0..ny {
            let ci = iy * nx + ix;
            let base = (ix * ny + iy) * hh;
            let mut z = 0usize;
            for (c, n) in d.runs_at(&off, ci) {
                for _ in 0..n {
                    let legacy = blocks[base + z];
                    if legacy == class::UNLOADED {
                        unloaded_cells += 1;
                    } else if legacy != c {
                        // The legacy harness classed non-solid sprites as air
                        // and solid ones as structure, and sprite placement is
                        // random per run, so these pairs are sprite noise,
                        // not terrain differences.
                        let sprite_noise = (legacy == class::STRUCTURE
                            && (c == class::AIR || c == class::SPRITE))
                            || (legacy == class::AIR && c == class::SPRITE);
                        if sprite_noise {
                            sprite_noise_cells += 1;
                        } else {
                            class_diff += 1;
                            first.get_or_insert((
                                x0 + ix as i32,
                                y0 + iy as i32,
                                zmin + z as i32,
                                legacy,
                                c,
                            ));
                        }
                    }
                    z += 1;
                }
            }
            let lc = &cols[(ix * ny + iy) * 16..][..16];
            let mine = [
                d.alt[ci],
                d.riverless_alt[ci],
                d.water_level[ci],
                d.warp_factor[ci],
            ];
            for k in 0..4 {
                let l =
                    f32::from_le_bytes([lc[k * 4], lc[k * 4 + 1], lc[k * 4 + 2], lc[k * 4 + 3]]);
                if l.to_bits() != mine[k].to_bits() {
                    col_diff[k] += 1;
                }
            }
        }
    }
    let total = (nx * ny * hh) as u64;
    println!("blocks compared      {total}");
    println!("class differences    {class_diff} (excluding sprite noise)");
    println!(
        "  (sprite-noise cells tolerated: {sprite_noise_cells}; legacy-unloaded skipped: \
         {unloaded_cells})"
    );
    println!(
        "column f32 differences (of {}): alt {}, riverless_alt {}, water_level {}, warp_factor {}",
        nx * ny,
        col_diff[0],
        col_diff[1],
        col_diff[2],
        col_diff[3]
    );
    if let Some((x, y, z, l, m)) = first {
        println!("first class difference at ({x},{y},{z}): legacy {l}, dump {m}");
    }
    let ok = class_diff == 0 && col_diff.iter().all(|&c| c == 0);
    println!("{}", if ok { "IDENTICAL" } else { "DIFFERENT" });
    Ok(ok)
}
