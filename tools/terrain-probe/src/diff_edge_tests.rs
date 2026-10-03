//! Edge cases of `terrain-probe diff` (the comparison that the discrepancy
//! test and the determinism checks rely on). Ids refer to the
//! precision-tooling corner-case catalogue.

use super::*;
use crate::format::{
    class,
    edge_tests::{Rng, dump_from_columns, random_dump},
};

/// `true` when the dumps are identical under the strict comparison.
fn compare(a: &Dump, b: &Dump, show: usize) -> Res<bool> {
    let o = DiffOpts {
        show,
        ..DiffOpts::default()
    };
    Ok(compare_with(a, b, &o)?.ok(&o))
}

/// EC-D50: identical dumps compare equal; a single changed block is found.
#[test]
fn ec_d50_single_block_difference_is_found() {
    let a = random_dump(&mut Rng::new(50), 5, 4, 30);
    assert!(compare(&a, &a.clone(), 0).unwrap());
    let cols: Vec<Vec<u8>> = vec![vec![class::GROUND; 5], vec![class::GROUND; 5]];
    let a = dump_from_columns([0, 0, 2, 1], 0, &cols);
    let mut c2 = cols.clone();
    c2[1][4] = class::AIR;
    let b = dump_from_columns([0, 0, 2, 1], 0, &c2);
    assert!(!compare(&a, &b, 5).unwrap());
}

/// EC-D51: the comparison is independent of how runs are split (a column
/// stored as one 70000-block run or as 65535 + 4465 is the same column).
#[test]
fn ec_d51_run_split_independence() {
    let a = dump_from_columns([0, 0, 1, 1], -40_000, &[vec![class::GROUND; 70_000]]);
    let mut b = a.clone();
    // Re-split: (G, 65535), (G, 4465) -> (G, 4465), (G, 65535).
    assert_eq!(b.run_len, vec![65535, 4465]);
    b.run_len = vec![4465, 65535];
    assert!(compare(&a, &b, 0).unwrap());
}

/// EC-D52: dumps of different boxes or z ranges are refused, not compared.
#[test]
fn ec_d52_different_boxes_are_refused() {
    let a = random_dump(&mut Rng::new(52), 3, 3, 10);
    let mut b = a.clone();
    b.header.box_xy[0] += 1;
    b.header.box_xy[2] += 1;
    assert!(compare(&a, &b, 0).is_err());
    let mut c = a.clone();
    c.header.zmin -= 1;
    assert!(compare(&a, &c, 0).is_err());
}

/// EC-D53 (BUG-P5, fixed): a difference only in the top block kind (Grass vs
/// Sand at the same z, both class GROUND) fails the strict comparison, so
/// `diff` exits 1 on any difference as the README promises.
#[test]
fn ec_d53_top_kind_difference_fails_the_diff() {
    let a = random_dump(&mut Rng::new(53), 2, 2, 8);
    let mut b = a.clone();
    b.top_kind[0] = b.top_kind[0].wrapping_add(1);
    assert!(!compare(&a, &b, 0).unwrap());
}

/// EC-D54: NaN floats compare by bit pattern: the same NaN is equal, a
/// different NaN payload is a difference (documents the rule).
#[test]
fn ec_d54_nan_floats_compare_by_bits() {
    let a = random_dump(&mut Rng::new(54), 2, 1, 4);
    assert!(a.water_level[0].is_nan());
    assert!(compare(&a, &a.clone(), 0).unwrap());
    let mut b = a.clone();
    b.water_level[0] = f32::from_bits(0x7fc0_0002);
    assert!(!compare(&a, &b, 0).unwrap());
}
