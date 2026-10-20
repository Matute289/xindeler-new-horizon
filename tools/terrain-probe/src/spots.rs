//! `terrain-probe spots`: list the procedural spot structures of a world window
//! as a `spots/v1` JSON document (the format of the open-world precision
//! tooling's `spots.py`): footprint boxes taken from the HOLLOW columns of each
//! placed prefab, and the deepest dent each box can make.
//!
//! The placement is the engine's own (`world::layer::spot::spot_footprint`,
//! same seed, rotation and structure choice as `apply_spots_to`); this module
//! only samples the terrain altitude for the slope spread and turns the result
//! into a document that obeys the format's limits.

use std::collections::BTreeMap;

use serde_json::{Value, json};
use vek::Vec2;
use world::{
    layer::spot::{chunk_spot_placement, spot_footprint, spot_label},
    util::Sampler,
};

use crate::probe::{Box2, Probe, Res};

/// Largest box side the format accepts (metres).
pub const MAX_BOX_SIDE_M: i32 = 48;
/// Largest `max_dig_blocks` the format accepts.
pub const MAX_DIG_BLOCKS: i32 = 8;
/// A spot's blit reaches 48 m from its chunk centre, so its chunk centre can be
/// up to 2 chunks outside the window.
const CHUNK_PAD: i32 = 2;

/// One placed spot, reduced to what the document needs.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Placement {
    /// `<spot kind>_<origin x>_<origin y>`: unique per placement.
    pub name: String,
    /// Hollow columns inside the window, each with the most blocks the spot
    /// can take off its top (from the terrain top of that very column).
    pub columns: Vec<(Vec2<i32>, i32)>,
}

/// The document and the placements it had to leave out.
#[derive(Debug)]
pub struct Built {
    pub doc: Value,
    /// Human-readable reports (placements rejected because they break a cap).
    pub warnings: Vec<String>,
}

/// Turn placements into a `spots/v1` document that always parses.
///
/// * A column hollowed by two spots is judged once, by the deeper one (ties go
///   to the earlier placement), so the boxes never overlap.
/// * A column whose dent is over `MAX_DIG_BLOCKS` is left out and reported (it
///   stays un-exempt, so a real defect cannot hide behind it).
/// * A placement is split by dent depth: `<name>_d<N>` boxes hold the columns
///   that can lose up to N blocks (N in 1..=8).
/// * Boxes are cut to at most `MAX_BOX_SIDE_M` per side; a placement that needs
///   several boxes gets `name_1`, `name_2`, ...
/// * `max_exempt_columns` is the number of hollow columns listed.
pub fn build_doc(arena: &str, source: &str, placements: &[Placement]) -> Built {
    let mut warnings = Vec::new();
    let mut ordered: Vec<&Placement> = placements.iter().collect();
    ordered.sort_by(|a, b| a.name.cmp(&b.name));
    // (y, x) -> (dig, placement index)
    // (y, x) -> (dig, placement index)
    let mut owner: BTreeMap<(i32, i32), (i32, usize)> = BTreeMap::new();
    for (i, p) in ordered.iter().enumerate() {
        let over: Vec<i32> = p
            .columns
            .iter()
            .map(|c| c.1)
            .filter(|&d| d > MAX_DIG_BLOCKS)
            .collect();
        if !over.is_empty() {
            warnings.push(format!(
                "rejected {} columns of {}: dent up to {} blocks is over the format limit \
                 {MAX_DIG_BLOCKS} (left un-exempt)",
                over.len(),
                p.name,
                over.iter().max().copied().unwrap_or(0)
            ));
        }
        for &(c, dig) in &p.columns {
            if !(1..=MAX_DIG_BLOCKS).contains(&dig) {
                continue;
            }
            let slot = owner.entry((c.y, c.x)).or_insert((dig, i));
            if dig > slot.0 {
                *slot = (dig, i);
            }
        }
    }
    let mut groups: BTreeMap<(usize, i32), Vec<Vec2<i32>>> = BTreeMap::new();
    for (&(y, x), &(dig, i)) in &owner {
        groups.entry((i, dig)).or_default().push(Vec2::new(x, y));
    }
    let mut spots = Vec::new();
    for ((i, dig), cols) in &groups {
        let boxes = world::layer::spot::column_boxes(cols.iter().copied(), MAX_BOX_SIDE_M);
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
    Built { doc, warnings }
}

/// Place every spot whose reach touches `bx` and reduce it to a [`Placement`]
/// (hollow columns inside `bx`, dent from the sampled terrain).
pub fn collect(p: &Probe, bx: Box2) -> Res<Vec<Placement>> {
    let sim = p.world.sim();
    let ir = p.index.as_index_ref();
    let cg = p.world.sample_columns();
    let (c0, c1) = bx.chunk_range();
    let inside = |c: Vec2<i32>| c.x >= bx.x0 && c.x < bx.x1 && c.y >= bx.y0 && c.y < bx.y1;
    let alt_of = |w: Vec2<i32>| cg.get((w, ir, None)).map(|c| c.alt as i32);
    let mut out = Vec::new();
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
            // The blit is anchored at the altitude of the chunk centre; each
            // column's own terrain top (relative to that block) says how many
            // of the hollow blocks below it are really cut out of its top.
            let origin_z = alt_of(wpos).unwrap_or(0);
            let cols: Vec<(Vec2<i32>, i32)> = fp
                .columns
                .iter()
                .filter(|c| inside(c.wpos))
                .map(|c| {
                    let top_rel = alt_of(c.wpos).unwrap_or(origin_z) - origin_z;
                    (c.wpos, c.dig_blocks(top_rel))
                })
                .collect();
            if cols.is_empty() {
                continue;
            }
            out.push(Placement {
                name: format!("{}_{}_{}", spot_label(&spot), wpos.x, wpos.y),
                columns: cols,
            });
        }
    }
    Ok(out)
}

/// Run `spots`: place, build, print warnings on stderr, return the document.
pub fn run(p: &Probe, bx: Box2, arena: &str) -> Res<Value> {
    let placements = collect(p, bx)?;
    let source = format!(
        "terrain-probe spots, engine {}, seed {}, window {},{},{},{} (hollow columns of the \
         placed prefabs)",
        env!("TPROBE_ENGINE_COMMIT"),
        p.seed,
        bx.x0,
        bx.y0,
        bx.x1,
        bx.y1
    );
    let built = build_doc(arena, &source, &placements);
    for w in &built.warnings {
        eprintln!("warning: {w}");
    }
    let n = built.doc["spots"].as_array().map_or(0, Vec::len);
    eprintln!(
        "{} spot placement(s) touch the window, {n} box(es) written",
        placements.len()
    );
    Ok(built.doc)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The rules of the consumer's `spots.parse`, as a validator.
    fn check_v1(doc: &Value) -> Result<(), String> {
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
                return Err("empty box".into());
            }
            if x1 - x0 > 48 || y1 - y0 > 48 {
                return Err("box side over 48".into());
            }
            let dig = it
                .get("max_dig_blocks")
                .and_then(Value::as_i64)
                .ok_or("dig")?;
            if !(1..=8).contains(&dig) {
                return Err("max_dig_blocks".into());
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
    fn overlapping_spots_are_merged_not_emitted_twice() {
        // Both hollow (11, 10); the deeper spot owns it.
        let b = build_doc("A", "t", &[
            place("a", &[(10, 10), (11, 10)], 2),
            place("b", &[(11, 10), (12, 10)], 3),
        ]);
        check_v1(&b.doc).unwrap();
        assert_eq!(b.doc["max_exempt_columns"], 3);
        let spots = b.doc["spots"].as_array().unwrap();
        let dig_of = |name: &str| {
            spots
                .iter()
                .find(|s| s["name"] == name)
                .map(|s| s["max_dig_blocks"].as_i64().unwrap())
        };
        assert_eq!(dig_of("a_d2"), Some(2));
        assert_eq!(dig_of("b_d3"), Some(3));
        let total: i64 = spots
            .iter()
            .map(|s| {
                let b = s["box"].as_array().unwrap();
                (b[2].as_i64().unwrap() - b[0].as_i64().unwrap())
                    * (b[3].as_i64().unwrap() - b[1].as_i64().unwrap())
            })
            .sum();
        assert_eq!(total, 3);
    }

    #[test]
    fn over_cap_and_harmless_placements_are_left_out_and_reported() {
        let b = build_doc("A", "t", &[
            place("deep", &[(0, 0)], 9),
            place("flat", &[(5, 5)], 0),
            place("ok", &[(9, 9)], 8),
        ]);
        check_v1(&b.doc).unwrap();
        assert_eq!(b.warnings.len(), 1);
        assert!(b.warnings[0].contains("deep"));
        assert_eq!(b.doc["spots"].as_array().unwrap().len(), 1);
        assert_eq!(b.doc["max_exempt_columns"], 1);
    }

    #[test]
    fn a_column_over_the_cap_is_left_out_but_the_rest_of_its_spot_stays() {
        let mut deep = place("pit", &[(0, 0), (1, 0), (2, 0)], 2);
        deep.columns[1].1 = 9; // one column on a rise: too deep
        deep.columns[2].1 = 1; // one column on a dip: shallow
        let b = build_doc("A", "t", &[deep]);
        check_v1(&b.doc).unwrap();
        assert_eq!(b.warnings.len(), 1);
        assert!(b.warnings[0].contains("pit") && b.warnings[0].contains("9"));
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
}
