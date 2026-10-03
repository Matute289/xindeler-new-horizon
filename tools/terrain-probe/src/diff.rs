//! Block-by-block comparison of two dumps of the same box.

use std::collections::BTreeMap;

use crate::{format::Dump, probe::Res};

/// Prints a summary and returns true when the two dumps hold identical
/// block classes and identical column floats.
pub fn compare(a: &Dump, b: &Dump, show: usize) -> Res<bool> {
    let (ha, hb) = (&a.header, &b.header);
    if ha.box_xy != hb.box_xy || ha.zmin != hb.zmin || ha.zmax != hb.zmax {
        return Err("dumps cover different boxes or z ranges".into());
    }
    let (oa, ob) = (a.run_offsets(), b.run_offsets());
    let nx = ha.nx as usize;
    let mut pairs: BTreeMap<(u8, u8), u64> = BTreeMap::new();
    let mut shown = Vec::new();
    let mut cols_with_diff = 0u64;
    let mut col_float_diff = 0u64;
    let mut surface_diff = 0u64;
    let (mut mn, mut mx) = ((i32::MAX, i32::MAX), (i32::MIN, i32::MIN));
    for i in 0..ha.columns() {
        let same_floats = a.alt[i].to_bits() == b.alt[i].to_bits()
            && a.riverless_alt[i].to_bits() == b.riverless_alt[i].to_bits()
            && a.water_level[i].to_bits() == b.water_level[i].to_bits()
            && a.warp_factor[i].to_bits() == b.warp_factor[i].to_bits();
        col_float_diff += u64::from(!same_floats);
        surface_diff += u64::from(
            a.ground_top[i] != b.ground_top[i]
                || a.water_top[i] != b.water_top[i]
                || a.liquid_depth[i] != b.liquid_depth[i]
                || a.top_kind[i] != b.top_kind[i],
        );
        // Fast skip: identical runs.
        let (ra, rb) = (
            &a.run_class[oa[i]..oa[i + 1]],
            &b.run_class[ob[i]..ob[i + 1]],
        );
        let (la, lb) = (&a.run_len[oa[i]..oa[i + 1]], &b.run_len[ob[i]..ob[i + 1]]);
        if ra == rb && la == lb {
            continue;
        }
        let mut z = ha.zmin;
        let mut col_diff = false;
        let (mut ia, mut ib) = (0usize, 0usize);
        let (mut left_a, mut left_b) = (u32::from(la[0]), u32::from(lb[0]));
        while ia < ra.len() && ib < rb.len() {
            let n = left_a.min(left_b);
            if ra[ia] != rb[ib] {
                col_diff = true;
                *pairs.entry((ra[ia], rb[ib])).or_default() += u64::from(n);
                if shown.len() < show {
                    shown.push((
                        ha.box_xy[0] + (i % nx) as i32,
                        ha.box_xy[1] + (i / nx) as i32,
                        z,
                        n,
                        ra[ia],
                        rb[ib],
                    ));
                }
            }
            z += n as i32;
            left_a -= n;
            left_b -= n;
            if left_a == 0 {
                ia += 1;
                left_a = la.get(ia).map_or(0, |&v| u32::from(v));
            }
            if left_b == 0 {
                ib += 1;
                left_b = lb.get(ib).map_or(0, |&v| u32::from(v));
            }
        }
        if col_diff {
            cols_with_diff += 1;
            let (x, y) = (
                ha.box_xy[0] + (i % nx) as i32,
                ha.box_xy[1] + (i / nx) as i32,
            );
            mn = (mn.0.min(x), mn.1.min(y));
            mx = (mx.0.max(x), mx.1.max(y));
        }
    }
    let total: u64 = pairs.values().sum();
    println!("columns with class differences  {cols_with_diff}");
    println!("blocks with class differences   {total}");
    println!("columns with float differences  {col_float_diff}");
    println!(
        "columns with surface differences (ground/water top, depth, top kind)  {surface_diff}"
    );
    for ((ca, cb), n) in &pairs {
        println!("  class {ca} -> {cb}: {n}");
    }
    if cols_with_diff > 0 {
        println!(
            "difference bbox x {}..={} y {}..={}",
            mn.0, mx.0, mn.1, mx.1
        );
    }
    for (x, y, z, n, ca, cb) in shown {
        println!("  ({x},{y}) z {z} +{n}: {ca} vs {cb}");
    }
    Ok(total == 0 && col_float_diff == 0)
}
