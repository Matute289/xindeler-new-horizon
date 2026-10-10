//! Manifest guards that run in every test build, whatever features are on.

use std::collections::{BTreeMap, BTreeSet};

/// Features that must only ever be enabled explicitly, never through another
/// feature (in particular `default` / `default-publish`).
const EXPLICIT_ONLY: &[&str] = &["shot-script"];

/// Parse `[features]` of a Cargo manifest into `name -> entries`. A careful
/// text scan (no TOML crate in this crate's dependencies): handles `#`
/// comments, multi-line arrays and quoted entries.
fn parse_features(manifest: &str) -> BTreeMap<String, Vec<String>> {
    let mut in_features = false;
    let mut features = BTreeMap::new();
    let mut current: Option<(String, String)> = None;
    for raw in manifest.lines() {
        // Feature entries never contain '#', so cutting at '#' is safe.
        let line = raw.split('#').next().unwrap_or("").trim();
        match current.as_mut() {
            Some((_, body)) => body.push_str(line),
            None => {
                if line.starts_with('[') {
                    in_features = line == "[features]";
                    continue;
                }
                if !in_features || line.is_empty() {
                    continue;
                }
                let (name, rest) = line
                    .split_once('=')
                    .unwrap_or_else(|| panic!("unexpected line in [features]: {raw:?}"));
                current = Some((name.trim().trim_matches('"').to_string(), rest.to_string()));
            },
        }
        if let Some((_, body)) = &current
            && body.contains(']')
        {
            let (name, body) = current.take().unwrap();
            let entries = body
                .split('"')
                .skip(1)
                .step_by(2)
                .map(str::to_string)
                .collect();
            features.insert(name, entries);
        }
    }
    assert!(current.is_none(), "unterminated feature array");
    features
}

/// Every feature reachable from `start` (excluding `start` itself).
fn reachable(features: &BTreeMap<String, Vec<String>>, start: &str) -> BTreeSet<String> {
    let mut seen = BTreeSet::new();
    let mut stack: Vec<&str> = features[start].iter().map(String::as_str).collect();
    while let Some(f) = stack.pop() {
        // `dep:x` / `dep/feat` / `dep?/feat` are not features of this crate.
        if f.contains(':') || f.contains('/') || !seen.insert(f.to_string()) {
            continue;
        }
        if let Some(next) = features.get(f) {
            stack.extend(next.iter().map(String::as_str));
        }
    }
    seen
}

#[test]
fn explicit_only_features_are_not_enabled_by_other_features() {
    let features = parse_features(include_str!("../Cargo.toml"));
    for set in ["default", "default-publish", "egui-ui"] {
        assert!(
            features.contains_key(set),
            "{set} feature missing: parser broken?"
        );
    }
    for explicit in EXPLICIT_ONLY {
        assert!(
            features.contains_key(*explicit),
            "{explicit} feature missing from voxygen/Cargo.toml"
        );
        for name in features.keys().filter(|n| n.as_str() != *explicit) {
            assert!(
                !reachable(&features, name).contains(*explicit),
                "feature `{name}` enables `{explicit}`, which must only be enabled explicitly"
            );
        }
    }
}

#[test]
fn feature_parser_follows_transitive_entries() {
    let manifest = "[package]\nname = \"x\"\n\n[features]\n# comment \"shot-script\"\na = [\"b\"] \
                    # trailing\nb = [\n    \"c\", # c\n    \"dep:z\",\n]\nc = \
                    []\n\n[dependencies]\nd = \"1\"\n";
    let f = parse_features(manifest);
    assert_eq!(f.len(), 3);
    assert_eq!(reachable(&f, "a"), ["b", "c"].map(String::from).into());
    assert!(reachable(&f, "c").is_empty());
}
