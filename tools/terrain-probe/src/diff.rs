//! Block-by-block comparison of two dumps of the same box.

use std::collections::BTreeMap;

use crate::{
    format::{ClientInfo, Dump, class},
    probe::Res,
};

/// How strict the comparison is.
#[derive(Clone, Debug, Default)]
pub struct DiffOpts {
    /// Print this many example differences.
    pub show: usize,
    /// Fast-vs-client mode: the client has no column floats (they are NaN in
    /// its dumps), terrain/water differences are only tolerated within
    /// `landing_radius` metres of a recorded bot landing point, and
    /// structure/sprite-only differences are reported but never fail.
    pub client_compare: bool,
    /// Horizontal radius (m) around each client landing point inside which a
    /// terrain/water difference is attributed to the bot's own landing.
    pub landing_radius: i32,
}

/// Result of [`compare_with`].
#[derive(Debug, Default)]
pub struct DiffReport {
    pub cols_with_diff: u64,
    pub blocks_with_diff: u64,
    pub col_float_diff: u64,
    pub surface_diff: u64,
    pub pairs: BTreeMap<(u8, u8), u64>,
    /// Blocks where either side is ground, liquid or unloaded.
    pub terrain_blocks: u64,
    pub terrain_cols: u64,
    /// The subset of the above farther than `landing_radius` from every
    /// landing point.
    pub terrain_blocks_outside: u64,
    pub terrain_cols_outside: u64,
    /// Differences between air, structure and sprite only.
    pub other_blocks: u64,
    pub other_cols: u64,
    pub bbox: Option<((i32, i32), (i32, i32))>,
    pub examples: Vec<(i32, i32, i32, u32, u8, u8)>,
}

impl DiffReport {
    /// Strict mode: nothing may differ (blocks, column floats, and the
    /// per-column surface summary including `top_kind`). Client mode: no
    /// terrain/water difference outside the landing radius.
    pub fn ok(&self, opts: &DiffOpts) -> bool {
        if opts.client_compare {
            self.terrain_blocks_outside == 0
        } else {
            self.blocks_with_diff == 0 && self.col_float_diff == 0 && self.surface_diff == 0
        }
    }

    pub fn print(&self, opts: &DiffOpts) {
        println!("columns with class differences  {}", self.cols_with_diff);
        println!("blocks with class differences   {}", self.blocks_with_diff);
        if opts.client_compare {
            println!("columns with float differences  (ignored: the client has no column floats)");
        } else {
            println!("columns with float differences  {}", self.col_float_diff);
        }
        println!(
            "columns with surface differences (ground/water top, depth, top kind)  {}",
            self.surface_diff
        );
        for ((ca, cb), n) in &self.pairs {
            println!("  class {ca} -> {cb}: {n}");
        }
        if let Some((mn, mx)) = self.bbox {
            println!(
                "difference bbox x {}..={} y {}..={}",
                mn.0, mx.0, mn.1, mx.1
            );
        }
        if opts.client_compare {
            println!(
                "terrain/water differences (ground, liquid or unloaded on either side): {} blocks \
                 in {} columns",
                self.terrain_blocks, self.terrain_cols
            );
            println!(
                "  of which farther than {} m from every landing point: {} blocks in {} columns",
                opts.landing_radius, self.terrain_blocks_outside, self.terrain_cols_outside
            );
            println!(
                "structure/sprite-only differences (air/structure/sprite): {} blocks in {} columns",
                self.other_blocks, self.other_cols
            );
        }
        for &(x, y, z, n, ca, cb) in &self.examples {
            println!("  ({x},{y}) z {z} +{n}: {ca} vs {cb}");
        }
    }
}

/// Landing points (x, y) recorded in whichever dump has client provenance.
pub fn landings(a: &Dump, b: &Dump) -> Vec<(f32, f32)> {
    let from = |c: &Option<ClientInfo>| -> Vec<(f32, f32)> {
        c.as_ref()
            .map(|c| {
                c.tiles
                    .iter()
                    .filter_map(|t| t.landing.map(|l| (l[0], l[1])))
                    .collect()
            })
            .unwrap_or_default()
    };
    let mut v = from(&a.header.client);
    v.extend(from(&b.header.client));
    v
}

fn terrainish(c: u8) -> bool { matches!(c, class::GROUND | class::LIQUID | class::UNLOADED) }

/// Compare two dumps of the same box and z range.
pub fn compare_with(a: &Dump, b: &Dump, opts: &DiffOpts) -> Res<DiffReport> {
    let (ha, hb) = (&a.header, &b.header);
    if ha.box_xy != hb.box_xy || ha.zmin != hb.zmin || ha.zmax != hb.zmax {
        return Err("dumps cover different boxes or z ranges".into());
    }
    let land = landings(a, b);
    let r2 = i64::from(opts.landing_radius).pow(2);
    let near_landing = |x: i32, y: i32| {
        land.iter().any(|&(lx, ly)| {
            let (dx, dy) = (
                f64::from(x) + 0.5 - f64::from(lx),
                f64::from(y) + 0.5 - f64::from(ly),
            );
            dx * dx + dy * dy <= r2 as f64
        })
    };
    let (oa, ob) = (a.run_offsets(), b.run_offsets());
    let nx = ha.nx as usize;
    let mut rep = DiffReport::default();
    let (mut mn, mut mx) = ((i32::MAX, i32::MAX), (i32::MIN, i32::MIN));
    for i in 0..ha.columns() {
        let same_floats = a.alt[i].to_bits() == b.alt[i].to_bits()
            && a.riverless_alt[i].to_bits() == b.riverless_alt[i].to_bits()
            && a.water_level[i].to_bits() == b.water_level[i].to_bits()
            && a.warp_factor[i].to_bits() == b.warp_factor[i].to_bits();
        rep.col_float_diff += u64::from(!same_floats);
        rep.surface_diff += u64::from(
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
        let (x, y) = (
            ha.box_xy[0] + (i % nx) as i32,
            ha.box_xy[1] + (i / nx) as i32,
        );
        let mut z = ha.zmin;
        let (mut col_diff, mut col_terrain, mut col_other) = (false, false, false);
        let mut col_outside = false;
        let (mut ia, mut ib) = (0usize, 0usize);
        let (mut left_a, mut left_b) = (u32::from(la[0]), u32::from(lb[0]));
        let mut near: Option<bool> = None;
        while ia < ra.len() && ib < rb.len() {
            let n = left_a.min(left_b);
            if ra[ia] != rb[ib] {
                col_diff = true;
                *rep.pairs.entry((ra[ia], rb[ib])).or_default() += u64::from(n);
                rep.blocks_with_diff += u64::from(n);
                if terrainish(ra[ia]) || terrainish(rb[ib]) {
                    col_terrain = true;
                    rep.terrain_blocks += u64::from(n);
                    let nl = *near.get_or_insert_with(|| near_landing(x, y));
                    if !nl {
                        col_outside = true;
                        rep.terrain_blocks_outside += u64::from(n);
                    }
                } else {
                    col_other = true;
                    rep.other_blocks += u64::from(n);
                }
                if rep.examples.len() < opts.show {
                    rep.examples.push((x, y, z, n, ra[ia], rb[ib]));
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
            rep.cols_with_diff += 1;
            rep.terrain_cols += u64::from(col_terrain);
            rep.terrain_cols_outside += u64::from(col_outside);
            rep.other_cols += u64::from(col_other);
            mn = (mn.0.min(x), mn.1.min(y));
            mx = (mx.0.max(x), mx.1.max(y));
        }
    }
    if rep.cols_with_diff > 0 {
        rep.bbox = Some((mn, mx));
    }
    Ok(rep)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::format::{ClientInfo, TileInfo, summarize};

    /// A 4x1 dump over z 0..4 where column `i` has the given runs.
    fn dump(cols: &[Vec<(u8, u16)>]) -> Dump {
        let n = cols.len();
        let mut d = Dump {
            header: crate::format::Header {
                format: crate::format::FORMAT_NAME.into(),
                box_xy: [0, 0, n as i32, 1],
                zmin: 0,
                zmax: 4,
                nx: n as u32,
                ny: 1,
                seed: 0,
                path: "fast".into(),
                calendar: None,
                engine_commit: String::new(),
                assets: BTreeMap::new(),
                class_codes: BTreeMap::new(),
                block_kinds: BTreeMap::new(),
                stats: BTreeMap::new(),
                client: None,
                sections: vec![],
            },
            alt: vec![f32::NAN; n],
            riverless_alt: vec![f32::NAN; n],
            water_level: vec![f32::NAN; n],
            warp_factor: vec![f32::NAN; n],
            top_kind: vec![255; n],
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
            let s = summarize(0, c);
            d.flags.push(s.flags);
            d.ground_top.push(s.ground_top);
            d.water_top.push(s.water_top);
            d.liquid_depth.push(s.liquid_depth);
            d.run_counts.push(c.len() as u16);
            for &(cl, len) in c {
                d.run_class.push(cl);
                d.run_len.push(len);
            }
        }
        d
    }

    fn with_landing(mut d: Dump, x: f32) -> Dump {
        d.header.path = "client".into();
        d.header.client = Some(ClientInfo {
            view_distance: 8,
            chunks_total: 1,
            chunks_streamed: 1,
            missing_chunks: vec![],
            tiles: vec![TileInfo {
                chunks: [0, 0, 0, 0],
                goto: [0, 0, 0],
                landing: Some([x, 0.5, 10.0]),
                streamed: 1,
                total: 1,
                secs: 0.0,
            }],
        });
        d
    }

    const G: u8 = class::GROUND;
    const A: u8 = class::AIR;
    const S: u8 = class::STRUCTURE;

    #[test]
    fn identical_dumps_pass_strict() {
        let c = vec![vec![(G, 2), (A, 2)]; 3];
        let (a, b) = (dump(&c), dump(&c));
        let o = DiffOpts::default();
        let r = compare_with(&a, &b, &o).unwrap();
        assert!(r.ok(&o) && r.blocks_with_diff == 0);
    }

    #[test]
    fn client_mode_tolerates_landing_and_sprites_but_not_far_terrain() {
        let base = vec![vec![(G, 2), (A, 2)]; 4];
        let mut other = base.clone();
        other[0] = vec![(G, 3), (A, 1)]; // terrain diff at x=0, near the landing
        other[3] = vec![(G, 2), (S, 1), (A, 1)]; // structure/air only, far away
        let a = dump(&base);
        let b = with_landing(dump(&other), 0.5);
        let o = DiffOpts {
            show: 5,
            client_compare: true,
            landing_radius: 1,
        };
        let r = compare_with(&a, &b, &o).unwrap();
        assert_eq!(r.terrain_blocks, 1);
        assert_eq!(r.terrain_blocks_outside, 0);
        assert_eq!(r.other_blocks, 1);
        assert!(r.ok(&o));
        // Strict mode fails on the same pair, and so does client mode once the
        // landing is moved away.
        assert!(!r.ok(&DiffOpts::default()));
        let b = with_landing(dump(&other), 3.5);
        let r = compare_with(&a, &b, &o).unwrap();
        assert_eq!(r.terrain_blocks_outside, 1);
        assert!(!r.ok(&o));
    }

    #[test]
    fn unloaded_counts_as_terrain() {
        let a = dump(&[vec![(G, 2), (A, 2)]]);
        let b = with_landing(dump(&[vec![(class::UNLOADED, 4)]]), 100.0);
        let o = DiffOpts {
            client_compare: true,
            landing_radius: 8,
            ..DiffOpts::default()
        };
        let r = compare_with(&a, &b, &o).unwrap();
        assert_eq!(r.terrain_blocks_outside, 4);
        assert!(!r.ok(&o));
    }
}

#[cfg(test)]
#[path = "diff_edge_tests.rs"]
mod edge_tests;
