//! `terrain-probe spots`: list the procedural spot structures of a world window
//! as a `spots/v1` JSON document (the format read by the external spots/v1
//! consumer): footprint boxes taken from the cleared columns of each placed
//! prefab, and the deepest dent each box can make.
//!
//! The placement is the engine's own (`world::layer::spot::spot_footprint`,
//! same seed, rotation and structure choice as `apply_spots_to`); this module
//! only samples the terrain altitude for the slope spread and turns the result
//! into a document that obeys the format's limits.
//!
//! The caps below (`MAX_BOX_SIDE_M`, `MAX_DIG_BLOCKS`, the key set, disjoint
//! boxes, unique names) are duplicated from the external consumer, which
//! rejects anything outside them. [`validate`] checks every emitted document
//! against them, and the golden fixture in the tests pins them.

use std::collections::{BTreeMap, BTreeSet};

use serde_json::{Value, json};
use vek::Vec2;
use world::{
    layer::spot::{chunk_spot_placement, spot_footprint},
    util::Sampler,
};

use crate::probe::{Box2, Probe, Res};

/// Largest box side the format accepts (metres).
pub const MAX_BOX_SIDE_M: i32 = 48;
/// Largest `max_dig_blocks` the format accepts.
pub const MAX_DIG_BLOCKS: i32 = 10;
/// A spot's blit reaches 48 m from its chunk centre, so its chunk centre can be
/// up to 2 chunks outside the window.
const CHUNK_PAD: i32 = 2;

/// One placed spot, reduced to what the document needs.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Placement {
    /// `<spot kind>_<origin x>_<origin y>`: unique per placement.
    pub name: String,
    /// Cleared columns inside the window, each with the most blocks the spot
    /// can take off its top (from the terrain top of that very column).
    pub columns: Vec<(Vec2<i32>, i32)>,
}

/// The document and what it had to leave out.
#[derive(Debug)]
pub struct Built {
    pub doc: Value,
    /// Human-readable reports (columns left out because they break a cap).
    pub warnings: Vec<String>,
    /// Columns left out because their dent is over the cap (alone or summed
    /// over overlapping spots). They stay un-exempt.
    pub dropped_columns: usize,
}

/// A spot short-label for names: the last segment of the structure group for a
/// RON-defined spot, the variant name otherwise.
fn spot_label(spot: &common::spot::Spot) -> String {
    match spot {
        common::spot::Spot::RonFile(p) => p
            .base_structures
            .rsplit('.')
            .next()
            .unwrap_or("ron_spot")
            .to_string(),
        other => format!("{other:?}"),
    }
}

/// Partition a set of world columns into disjoint half-open boxes
/// `[x0, y0, x1, y1]` that cover exactly those columns (no column outside the
/// set), with every side at most `max_side` (greedy: grow along x, then along
/// y). Deterministic.
fn column_boxes(cols: impl IntoIterator<Item = Vec2<i32>>, max_side: i32) -> Vec<[i32; 4]> {
    let max_side = max_side.max(1);
    // Keyed (y, x) so the iteration order is row by row.
    let free: BTreeSet<(i32, i32)> = cols.into_iter().map(|c| (c.y, c.x)).collect();
    let mut used: BTreeSet<(i32, i32)> = BTreeSet::new();
    let mut out = Vec::new();
    for &(y, x) in &free {
        if used.contains(&(y, x)) {
            continue;
        }
        let ok = |y: i32, x: i32, used: &BTreeSet<(i32, i32)>| {
            free.contains(&(y, x)) && !used.contains(&(y, x))
        };
        let mut x1 = x + 1;
        while x1 - x < max_side && ok(y, x1, &used) {
            x1 += 1;
        }
        let mut y1 = y + 1;
        while y1 - y < max_side && (x..x1).all(|xx| ok(y1, xx, &used)) {
            y1 += 1;
        }
        for yy in y..y1 {
            for xx in x..x1 {
                used.insert((yy, xx));
            }
        }
        out.push([x, y, x1, y1]);
    }
    out
}

/// Turn placements into a `spots/v1` document that always parses.
///
/// * A column cleared by several spots gets the SUM of their dents (each spot
///   cuts from the top in turn), owned by the first placement by name, so the
///   boxes never overlap. If the sum is over `MAX_DIG_BLOCKS` the column is
///   left out and reported.
/// * A column whose dent is over `MAX_DIG_BLOCKS` for any single spot is left
///   out (for every spot) and reported: it stays un-exempt, so a real defect
///   cannot hide behind it.
/// * A placement is split by dent depth: `<name>_d<N>` boxes hold the columns
///   that can lose up to N blocks (N in 1..=10).
/// * Boxes are cut to at most `MAX_BOX_SIDE_M` per side; a placement that needs
///   several boxes gets `name_1`, `name_2`, ...
/// * `max_exempt_columns` is the number of cleared columns listed.
pub fn build_doc(arena: &str, source: &str, placements: &[Placement]) -> Built {
    let mut warnings = Vec::new();
    let mut ordered: Vec<&Placement> = placements.iter().collect();
    ordered.sort_by(|a, b| a.name.cmp(&b.name));

    // Columns any single spot would dig deeper than the cap.
    let mut poisoned: BTreeSet<(i32, i32)> = BTreeSet::new();
    for p in &ordered {
        let over: Vec<(Vec2<i32>, i32)> = p
            .columns
            .iter()
            .filter(|c| c.1 > MAX_DIG_BLOCKS)
            .copied()
            .collect();
        if !over.is_empty() {
            warnings.push(format!(
                "rejected {} columns of {}: dent up to {} blocks is over the format limit \
                 {MAX_DIG_BLOCKS} (left un-exempt)",
                over.len(),
                p.name,
                over.iter().map(|c| c.1).max().unwrap_or(0)
            ));
            poisoned.extend(over.iter().map(|c| (c.0.y, c.0.x)));
        }
    }

    // (y, x) -> (summed dig, owning placement index)
    // Overlapping dents are summed per column. The sum is neither an upper nor
    // a lower bound of the real dig: voxels already cleared by another spot
    // are counted twice (overcount), and a cavity that lies below another
    // spot's dent is missed (undercount). Both errors fail loudly (a false
    // defect, or `--strict`) and never hide a defect.
    let mut owner: BTreeMap<(i32, i32), (i32, usize)> = BTreeMap::new();
    for (i, p) in ordered.iter().enumerate() {
        for &(c, dig) in &p.columns {
            let key = (c.y, c.x);
            if !(1..=MAX_DIG_BLOCKS).contains(&dig) || poisoned.contains(&key) {
                continue;
            }
            let slot = owner.entry(key).or_insert((0, i));
            slot.0 += dig;
        }
    }
    let summed_over: Vec<(i32, i32)> = owner
        .iter()
        .filter(|(_, v)| v.0 > MAX_DIG_BLOCKS)
        .map(|(k, _)| *k)
        .collect();
    if !summed_over.is_empty() {
        warnings.push(format!(
            "rejected {} columns cleared by overlapping spots: the dents add up to more than the \
             format limit {MAX_DIG_BLOCKS} (left un-exempt), first at x={} y={}",
            summed_over.len(),
            summed_over[0].1,
            summed_over[0].0
        ));
        for k in &summed_over {
            owner.remove(k);
        }
    }
    let dropped_columns = poisoned.len() + summed_over.len();

    let mut groups: BTreeMap<(usize, i32), Vec<Vec2<i32>>> = BTreeMap::new();
    for (&(y, x), &(dig, i)) in &owner {
        groups.entry((i, dig)).or_default().push(Vec2::new(x, y));
    }
    let mut spots = Vec::new();
    for ((i, dig), cols) in &groups {
        let boxes = column_boxes(cols.iter().copied(), MAX_BOX_SIDE_M);
        let base = format!("{}_d{dig}", ordered[*i].name);
        for (k, b) in boxes.iter().enumerate() {
            let name = if boxes.len() == 1 {
                base.clone()
            } else {
                format!("{base}_{}", k + 1)
            };
            spots.push(json!({"name": name, "box": b, "max_dig_blocks": dig}));
        }
    }
    let mut doc = json!({
        "format": "spots/v1",
        "arena": arena,
        "source": source,
        "spots": spots,
    });
    if !owner.is_empty() {
        doc["max_exempt_columns"] = json!(owner.len());
    }
    Built {
        doc,
        warnings,
        dropped_columns,
    }
}

/// The rules of the external consumer's parser, as a validator: the document
/// must have exactly the allowed keys, boxes of side at most `MAX_BOX_SIDE_M`,
/// `max_dig_blocks` in `1..=MAX_DIG_BLOCKS`, disjoint boxes and unique names.
pub fn validate(doc: &Value) -> Result<(), String> {
    let o = doc.as_object().ok_or("not an object")?;
    if o.get("format") != Some(&json!("spots/v1")) {
        return Err("format".into());
    }
    for k in o.keys() {
        if !["format", "arena", "source", "spots", "max_exempt_columns"].contains(&k.as_str()) {
            return Err(format!("unknown key {k}"));
        }
    }
    for k in ["arena", "source"] {
        if o.get(k).and_then(Value::as_str).is_none_or(str::is_empty) {
            return Err(format!("{k} must be a non-empty string"));
        }
    }
    if let Some(c) = o.get("max_exempt_columns")
        && c.as_i64().is_none_or(|c| c < 1)
    {
        return Err("max_exempt_columns".into());
    }
    let items = o.get("spots").and_then(Value::as_array).ok_or("spots")?;
    let mut seen: Vec<(String, [i64; 4])> = Vec::new();
    for it in items {
        let it = it.as_object().ok_or("spot not an object")?;
        if it.len() != 3 {
            return Err("spot needs exactly name, box, max_dig_blocks".into());
        }
        let name = it
            .get("name")
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty());
        let name = name.ok_or("name")?.to_string();
        let bx: Vec<i64> = it
            .get("box")
            .and_then(Value::as_array)
            .ok_or("box")?
            .iter()
            .map(|v| v.as_i64().ok_or("box int"))
            .collect::<Result<_, _>>()?;
        let [x0, y0, x1, y1] = bx[..] else {
            return Err("box needs 4".into());
        };
        if x1 <= x0 || y1 <= y0 {
            return Err(format!("{name}: empty box"));
        }
        if x1 - x0 > i64::from(MAX_BOX_SIDE_M) || y1 - y0 > i64::from(MAX_BOX_SIDE_M) {
            return Err(format!("{name}: box side over {MAX_BOX_SIDE_M}"));
        }
        let dig = it
            .get("max_dig_blocks")
            .and_then(Value::as_i64)
            .ok_or("dig")?;
        if !(1..=i64::from(MAX_DIG_BLOCKS)).contains(&dig) {
            return Err(format!("{name}: max_dig_blocks"));
        }
        for (n, o) in &seen {
            if *n == name {
                return Err(format!("duplicate {name}"));
            }
            if x0 < o[2] && o[0] < x1 && y0 < o[3] && o[1] < y1 {
                return Err(format!("{name} overlaps {n}"));
            }
        }
        seen.push((name, [x0, y0, x1, y1]));
    }
    Ok(())
}

/// Placements and the column samples that failed while building them.
#[derive(Debug, Default)]
pub struct Collected {
    pub placements: Vec<Placement>,
    /// One report per terrain sample that could not be taken. The affected
    /// column (or the whole placement, if its origin failed) is NOT listed.
    pub sample_failures: Vec<String>,
}

/// Place every spot whose reach touches `bx` and reduce it to a [`Placement`]
/// (cleared columns inside `bx`, dent from the sampled terrain).
pub fn collect(p: &Probe, bx: Box2) -> Res<Collected> {
    let sim = p.world.sim();
    let ir = p.index.as_index_ref();
    let cg = p.world.sample_columns();
    let (c0, c1) = bx.chunk_range();
    let inside = |c: Vec2<i32>| c.x >= bx.x0 && c.x < bx.x1 && c.y >= bx.y0 && c.y < bx.y1;
    let alt_of = |w: Vec2<i32>| cg.get((w, ir, None)).map(|c| c.alt as i32);
    let mut out = Collected::default();
    for cy in c0.y - CHUNK_PAD..=c1.y + CHUNK_PAD {
        for cx in c0.x - CHUNK_PAD..=c1.x + CHUNK_PAD {
            let pos = Vec2::new(cx, cy);
            let Some(spot) = sim.get(pos).and_then(|c| c.spot) else {
                continue;
            };
            let (wpos, seed) = chunk_spot_placement(pos);
            let Some(fp) = spot_footprint(wpos, &spot, seed) else {
                continue;
            };
            let name = format!("{}_{}_{}", spot_label(&spot), wpos.x, wpos.y);
            // The blit is anchored at the altitude of the chunk centre; each
            // column's own terrain top (relative to that block) says how many
            // of the cleared blocks below it are really cut out of its top.
            let Some(origin_z) = alt_of(wpos) else {
                out.sample_failures.push(format!(
                    "{name}: no terrain sample at its origin ({}, {}); the placement is left out",
                    wpos.x, wpos.y
                ));
                continue;
            };
            let mut cols: Vec<(Vec2<i32>, i32)> = Vec::new();
            for c in fp.columns.iter().filter(|c| inside(c.wpos)) {
                match alt_of(c.wpos) {
                    Some(alt) => cols.push((c.wpos, c.dig_blocks(alt - origin_z))),
                    None => out.sample_failures.push(format!(
                        "{name}: no terrain sample at column ({}, {}); the column is left out",
                        c.wpos.x, c.wpos.y
                    )),
                }
            }
            if cols.is_empty() {
                continue;
            }
            out.placements.push(Placement {
                name,
                columns: cols,
            });
        }
    }
    Ok(out)
}

/// What `run` produced.
#[derive(Debug)]
pub struct Run {
    pub doc: Value,
    /// Columns left out for being deeper than the cap.
    pub dropped_columns: usize,
    /// Terrain samples that failed (columns / placements left out).
    pub sample_failures: usize,
}

impl Run {
    /// Whether anything was left out of the document.
    pub fn is_incomplete(&self) -> bool { self.dropped_columns > 0 || self.sample_failures > 0 }
}

/// Run `spots`: place, build, validate, print warnings on stderr.
pub fn run(p: &Probe, bx: Box2, arena: &str) -> Res<Run> {
    let collected = collect(p, bx)?;
    let source = format!(
        "terrain-probe spots, engine {}, seed {}, window {},{},{},{} (cleared columns of the \
         placed prefabs)",
        env!("TPROBE_ENGINE_COMMIT"),
        p.seed,
        bx.x0,
        bx.y0,
        bx.x1,
        bx.y1
    );
    let built = build_doc(arena, &source, &collected.placements);
    validate(&built.doc)
        .map_err(|e| format!("internal error: emitted document is invalid: {e}"))?;
    for w in &collected.sample_failures {
        eprintln!("error: {w}");
    }
    for w in &built.warnings {
        eprintln!("warning: {w}");
    }
    let n = built.doc["spots"].as_array().map_or(0, Vec::len);
    eprintln!(
        "{} spot placement(s) touch the window, {n} box(es) written, {} column(s) left out for \
         being deeper than {MAX_DIG_BLOCKS} blocks, {} terrain sample(s) failed",
        collected.placements.len(),
        built.dropped_columns,
        collected.sample_failures.len()
    );
    Ok(Run {
        doc: built.doc,
        dropped_columns: built.dropped_columns,
        sample_failures: collected.sample_failures.len(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn check_v1(doc: &Value) -> Result<(), String> { validate(doc) }

    fn place(name: &str, cols: &[(i32, i32)], dig: i32) -> Placement {
        Placement {
            name: name.into(),
            columns: cols.iter().map(|&(x, y)| (Vec2::new(x, y), dig)).collect(),
        }
    }

    #[test]
    fn emitted_document_obeys_the_documented_schema() {
        let strip: Vec<(i32, i32)> = (0..100).map(|x| (x, 5)).collect();
        let b = build_doc("A-01", "test", &[
            place("grave", &[(10, 10), (11, 10)], 2),
            place("wide", &strip, 3),
            place("pit", &[(500, 500)], 4),
        ]);
        check_v1(&b.doc).unwrap();
        assert!(b.warnings.is_empty());
        assert_eq!(b.doc["format"], "spots/v1");
        assert_eq!(b.doc["arena"], "A-01");
        // 2 + 100 + 1 hollow columns.
        assert_eq!(b.doc["max_exempt_columns"], 103);
        // The strip was cut at 48 m: three boxes, all named apart.
        let names: Vec<_> = b.doc["spots"]
            .as_array()
            .unwrap()
            .iter()
            .map(|s| s["name"].as_str().unwrap().to_string())
            .collect();
        assert!(names.contains(&"grave_d2".to_string()));
        assert!(
            names.contains(&"wide_d3_1".to_string()) && names.contains(&"wide_d3_3".to_string())
        );
        // It round-trips through JSON text unchanged.
        let text = serde_json::to_string(&b.doc).unwrap();
        let back: Value = serde_json::from_str(&text).unwrap();
        assert_eq!(back, b.doc);
        check_v1(&back).unwrap();
    }

    #[test]
    fn overlapping_spots_add_their_dents_on_the_shared_column() {
        // Both clear (11, 10): 2 + 3 blocks, owned by the first placement.
        let b = build_doc("A", "t", &[
            place("a", &[(10, 10), (11, 10)], 2),
            place("b", &[(11, 10), (12, 10)], 3),
        ]);
        check_v1(&b.doc).unwrap();
        assert!(b.warnings.is_empty());
        assert_eq!(b.dropped_columns, 0);
        assert_eq!(b.doc["max_exempt_columns"], 3);
        let spots = b.doc["spots"].as_array().unwrap();
        let box_of = |name: &str| {
            spots
                .iter()
                .find(|s| s["name"] == name)
                .map(|s| (s["box"].clone(), s["max_dig_blocks"].as_i64().unwrap()))
        };
        assert_eq!(box_of("a_d2"), Some((json!([10, 10, 11, 11]), 2)));
        assert_eq!(box_of("a_d5"), Some((json!([11, 10, 12, 11]), 5)));
        assert_eq!(box_of("b_d3"), Some((json!([12, 10, 13, 11]), 3)));
        assert_eq!(spots.len(), 3);
    }

    #[test]
    fn overlapping_dents_over_the_cap_are_left_out_and_reported() {
        let b = build_doc("A", "t", &[
            place("a", &[(10, 10), (11, 10)], 6),
            place("b", &[(11, 10)], 5),
        ]);
        check_v1(&b.doc).unwrap();
        assert_eq!(b.dropped_columns, 1);
        assert_eq!(b.warnings.len(), 1);
        assert!(b.warnings[0].contains("overlapping") && b.warnings[0].contains("x=11 y=10"));
        assert_eq!(b.doc["max_exempt_columns"], 1);
        assert_eq!(b.doc["spots"].as_array().unwrap().len(), 1);
    }

    #[test]
    fn a_column_over_the_cap_for_one_spot_is_left_out_for_all() {
        let mut deep = place("deep", &[(0, 0)], 11);
        deep.columns[0].1 = 11;
        let b = build_doc("A", "t", &[deep, place("zz", &[(0, 0), (1, 0)], 2)]);
        check_v1(&b.doc).unwrap();
        assert_eq!(b.dropped_columns, 1);
        // Only (1, 0) survives: the shared column does not hide behind zz.
        assert_eq!(b.doc["max_exempt_columns"], 1);
    }

    #[test]
    fn over_cap_and_harmless_placements_are_left_out_and_reported() {
        let b = build_doc("A", "t", &[
            place("deep", &[(0, 0)], 11),
            place("flat", &[(5, 5)], 0),
            place("ok", &[(9, 9)], 10),
        ]);
        check_v1(&b.doc).unwrap();
        assert_eq!(b.warnings.len(), 1);
        assert_eq!(b.dropped_columns, 1);
        assert!(b.warnings[0].contains("deep"));
        assert_eq!(b.doc["spots"].as_array().unwrap().len(), 1);
        assert_eq!(b.doc["max_exempt_columns"], 1);
    }

    #[test]
    fn a_column_over_the_cap_is_left_out_but_the_rest_of_its_spot_stays() {
        let mut deep = place("pit", &[(0, 0), (1, 0), (2, 0)], 2);
        deep.columns[1].1 = 11; // one column on a rise: too deep
        deep.columns[2].1 = 1; // one column on a dip: shallow
        let b = build_doc("A", "t", &[deep]);
        check_v1(&b.doc).unwrap();
        assert_eq!(b.warnings.len(), 1);
        assert!(b.warnings[0].contains("pit") && b.warnings[0].contains("11"));
        assert_eq!(b.doc["max_exempt_columns"], 2);
        let digs: Vec<i64> = b.doc["spots"]
            .as_array()
            .unwrap()
            .iter()
            .map(|s| s["max_dig_blocks"].as_i64().unwrap())
            .collect();
        assert_eq!(digs.len(), 2);
        assert!(digs.contains(&1) && digs.contains(&2));
    }

    #[test]
    fn an_empty_window_gives_a_valid_empty_document() {
        let b = build_doc("A", "t", &[]);
        check_v1(&b.doc).unwrap();
        assert!(b.doc.get("max_exempt_columns").is_none());
    }

    /// A `spots/v1` document as the external consumer
    /// (`tools/precision/spots.py` in the open-world repo) accepts it. The
    /// caps asserted below (48 m box side, 1..=10 dig blocks, disjoint
    /// boxes, unique names, exactly the allowed keys) live in that consumer
    /// too; if they change there, change them here and in `MAX_BOX_SIDE_M`
    /// / `MAX_DIG_BLOCKS` together.
    const GOLDEN: &str = r#"{
  "format": "spots/v1",
  "arena": "A-01",
  "source": "golden fixture",
  "max_exempt_columns": 102,
  "spots": [
    {"name": "grave_small_10_10_d2", "box": [10, 10, 12, 11], "max_dig_blocks": 2},
    {"name": "grave_small_10_10_d5", "box": [12, 10, 13, 11], "max_dig_blocks": 5},
    {"name": "wide_0_0_d8_1", "box": [100, 0, 148, 2], "max_dig_blocks": 8},
    {"name": "wide_0_0_d8_2", "box": [148, 0, 149, 2], "max_dig_blocks": 8},
    {"name": "wide_0_0_d1", "box": [200, 200, 201, 201], "max_dig_blocks": 1}
  ]
}"#;

    #[test]
    fn golden_document_passes_the_validator_with_the_caps() {
        let doc: Value = serde_json::from_str(GOLDEN).unwrap();
        validate(&doc).unwrap();
        assert_eq!((MAX_BOX_SIDE_M, MAX_DIG_BLOCKS), (48, 10));
        // The caps are tight: the first box side at the cap, the first dig at
        // the cap, and one step past either is refused.
        let mut wide = doc.clone();
        wide["spots"][2]["box"] = json!([100, 0, 149, 2]);
        assert!(validate(&wide).unwrap_err().contains("box side over 48"));
        let mut at_cap = doc.clone();
        at_cap["spots"][2]["max_dig_blocks"] = json!(10);
        validate(&at_cap).unwrap();
        let mut deep = doc.clone();
        deep["spots"][2]["max_dig_blocks"] = json!(11);
        assert!(validate(&deep).unwrap_err().contains("max_dig_blocks"));
        let mut none = doc.clone();
        none["spots"][0]["max_dig_blocks"] = json!(0);
        assert!(validate(&none).unwrap_err().contains("max_dig_blocks"));
        let mut overlap = doc.clone();
        overlap["spots"][1]["box"] = json!([11, 10, 13, 11]);
        assert!(validate(&overlap).unwrap_err().contains("overlaps"));
        let mut dup = doc.clone();
        dup["spots"][1]["name"] = json!("grave_small_10_10_d2");
        assert!(validate(&dup).unwrap_err().contains("duplicate"));
        let mut extra = doc.clone();
        extra["dropped_columns"] = json!(3);
        assert!(validate(&extra).unwrap_err().contains("unknown key"));
    }

    #[test]
    fn built_document_matches_the_golden_fixture() {
        let strip: Vec<(i32, i32)> = (100..149).flat_map(|x| [(x, 0), (x, 1)]).collect();
        let b = build_doc("A-01", "golden fixture", &[
            place("grave_small_10_10", &[(10, 10), (11, 10)], 2),
            Placement {
                name: "grave_small_10_10_b".into(),
                columns: vec![(Vec2::new(12, 10), 5)],
            },
            place("wide_0_0", &strip, 8),
            place("wide_0_0_z", &[(200, 200)], 1),
        ]);
        // Names differ from the fixture's by construction; compare the boxes
        // and digs, which are what the consumer reads.
        let got: Vec<(Value, Value)> = b.doc["spots"]
            .as_array()
            .unwrap()
            .iter()
            .map(|s| (s["box"].clone(), s["max_dig_blocks"].clone()))
            .collect();
        let want: Vec<(Value, Value)> = serde_json::from_str::<Value>(GOLDEN).unwrap()["spots"]
            .as_array()
            .unwrap()
            .iter()
            .map(|s| (s["box"].clone(), s["max_dig_blocks"].clone()))
            .collect();
        assert_eq!(got.len(), want.len());
        for w in &want {
            assert!(got.contains(w), "missing {w:?} in {got:?}");
        }
        validate(&b.doc).unwrap();
        assert_eq!(b.doc["max_exempt_columns"], 2 + 1 + 98 + 1);
    }

    #[test]
    fn column_boxes_cover_exactly_the_columns_and_are_disjoint_and_small() {
        let strip: Vec<_> = (0..130)
            .flat_map(|x| (0..3).map(move |y| Vec2::new(x, y)))
            .collect();
        let boxes = column_boxes(strip.iter().copied(), 48);
        let mut covered = BTreeSet::new();
        for b in &boxes {
            assert!(b[2] - b[0] <= 48 && b[3] - b[1] <= 48, "{b:?}");
            for y in b[1]..b[3] {
                for x in b[0]..b[2] {
                    assert!(covered.insert((x, y)), "overlap at {x},{y}");
                }
            }
        }
        assert_eq!(covered.len(), 130 * 3);
        // An L shape is not boxed as its bounding rectangle.
        let l = [Vec2::new(0, 0), Vec2::new(1, 0), Vec2::new(0, 1)];
        let boxes = column_boxes(l, 48);
        assert_eq!(
            boxes
                .iter()
                .map(|b| (b[2] - b[0]) * (b[3] - b[1]))
                .sum::<i32>(),
            3
        );
    }

    #[test]
    fn incomplete_runs_are_flagged_for_strict() {
        let run = |dropped_columns, sample_failures| Run {
            doc: json!({}),
            dropped_columns,
            sample_failures,
        };
        assert!(!run(0, 0).is_incomplete());
        assert!(run(1, 0).is_incomplete());
        assert!(run(0, 1).is_incomplete());
    }
}
