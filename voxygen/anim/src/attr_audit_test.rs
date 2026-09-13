//! Roster-wide audit of `SkeletonAttr` fall-throughs.
//!
//! See [`super::attr_audit`] for the mechanism. Most `SkeletonAttr` fields are
//! exhaustive matches, so a new species cannot compile without them. This test
//! covers the minority that are not: the ones a new species can silently
//! inherit, giving a creature that renders at the wrong scale with the wrong
//! gait and no diagnostic anywhere.
//!
//! The observed set is diffed against `attr_fallback_ledger.txt`; the failure
//! message prints the exact lines to add or delete.

use common::comp::{
    arthropod, biped_large, biped_small, bird_large, bird_medium, crustacean, dragon, fish_medium,
    fish_small, golem, humanoid, object, quadruped_low, quadruped_medium, quadruped_small, ship,
    theropod,
};

use super::attr_audit::probe;

/// Collects `"<BodyKind>.<attr> <variant>"` for every fall-through that fires
/// while building each body's `SkeletonAttr`.
fn observed_fallbacks() -> Vec<String> {
    let mut hits: Vec<String> = Vec::new();

    // Records one body: builds its `SkeletonAttr` and notes every audited
    // field that fell through to the body kind's catch-all.
    macro_rules! record {
        ($kind:literal, $label:expr, $attr_ty:ty, $body:expr) => {{
            let body = $body;
            for (body_kind, attr) in probe(|| <$attr_ty>::from(&body)) {
                assert_eq!(
                    body_kind, $kind,
                    "`attr_fallback!` in {} is labelled with the wrong body kind",
                    $kind
                );
                assert!(
                    AUDITED_FIELDS.contains(&($kind, attr)),
                    "{}.{attr} fell through but is missing from AUDITED_FIELDS — add it, or the \
                     ledger will not describe it",
                    $kind,
                );
                hits.push(format!("{}.{} {}", $kind, attr, $label));
            }
        }};
    }

    macro_rules! species_bodies {
        ($kind:literal, $module:ident, $attr_ty:ty) => {
            for species in $module::ALL_SPECIES {
                for body_type in $module::ALL_BODY_TYPES {
                    record!(
                        $kind,
                        format!("{species:?}/{body_type:?}"),
                        $attr_ty,
                        $module::Body { species, body_type }
                    );
                }
            }
        };
    }

    // Body kinds with no wrapped wildcard are walked too: it costs nothing and
    // it means a fall-through wrapped in one of them later cannot pass
    // vacuously because nothing ever constructed that body.
    species_bodies!("Arthropod", arthropod, crate::arthropod::SkeletonAttr);
    species_bodies!("BipedLarge", biped_large, crate::biped_large::SkeletonAttr);
    species_bodies!("BipedSmall", biped_small, crate::biped_small::SkeletonAttr);
    species_bodies!("BirdLarge", bird_large, crate::bird_large::SkeletonAttr);
    species_bodies!("BirdMedium", bird_medium, crate::bird_medium::SkeletonAttr);
    species_bodies!("Crustacean", crustacean, crate::crustacean::SkeletonAttr);
    species_bodies!("Dragon", dragon, crate::dragon::SkeletonAttr);
    species_bodies!("FishMedium", fish_medium, crate::fish_medium::SkeletonAttr);
    species_bodies!("FishSmall", fish_small, crate::fish_small::SkeletonAttr);
    species_bodies!("Golem", golem, crate::golem::SkeletonAttr);
    species_bodies!("Theropod", theropod, crate::theropod::SkeletonAttr);
    species_bodies!(
        "QuadrupedLow",
        quadruped_low,
        crate::quadruped_low::SkeletonAttr
    );
    species_bodies!(
        "QuadrupedMedium",
        quadruped_medium,
        crate::quadruped_medium::SkeletonAttr
    );
    species_bodies!(
        "QuadrupedSmall",
        quadruped_small,
        crate::quadruped_small::SkeletonAttr
    );

    // `Object` and `Ship` are flat body enums rather than species/body-type
    // pairs, but their `SkeletonAttr`s default the same way — and `Body::Object`
    // is the documented two-bone escape hatch for props, projectiles and spell
    // objects, so it is exactly the path a new asset is most likely to take.
    for obj in object::ALL_OBJECTS {
        record!(
            "Object",
            format!("{obj:?}"),
            crate::object::SkeletonAttr,
            obj
        );
    }
    for ship in ship::ALL_BODIES {
        record!("Ship", format!("{ship:?}"), crate::ship::SkeletonAttr, ship);
    }
    for body in humanoid::Body::iter() {
        record!(
            "Character",
            format!("{:?}/{:?}", body.species, body.body_type),
            crate::character::SkeletonAttr,
            body
        );
    }

    hits.sort();
    hits.dedup();
    hits
}

/// Every `(body kind, field)` whose `SkeletonAttr` match ends in a wildcard.
///
/// Kept explicit rather than derived so that *removing* an `attr_fallback!`
/// without updating the ledger is caught: the field stops being counted and
/// its ledger lines show up as stale.
const AUDITED_FIELDS: &[(&str, &str)] = &[
    ("Arthropod", "leg_ori"),
    ("Arthropod", "snapper"),
    ("BipedLarge", "tail"),
    ("BipedLarge", "tempo"),
    ("BipedLarge", "shl"),
    ("BipedLarge", "shr"),
    ("BipedLarge", "sc"),
    ("BipedLarge", "hhl"),
    ("BipedLarge", "hhr"),
    ("BipedLarge", "hc"),
    ("BipedLarge", "sthl"),
    ("BipedLarge", "sthr"),
    ("BipedLarge", "stc"),
    ("BipedLarge", "bhl"),
    ("BipedLarge", "bhr"),
    ("BipedLarge", "bc"),
    ("Object", "bone0"),
    ("Object", "bone1"),
    ("QuadrupedLow", "side_head_lower"),
    ("QuadrupedLow", "side_head_upper"),
    ("QuadrupedLow", "lean"),
    ("QuadrupedLow", "scaler"),
    ("QuadrupedLow", "tempo"),
    ("QuadrupedMedium", "scaler"),
    ("QuadrupedMedium", "startangle"),
    ("QuadrupedMedium", "tempo"),
    ("QuadrupedMedium", "spring"),
    ("QuadrupedMedium", "feed"),
    ("QuadrupedSmall", "scaler"),
    ("QuadrupedSmall", "tempo"),
    ("QuadrupedSmall", "maximize"),
    ("QuadrupedSmall", "minimize"),
    ("QuadrupedSmall", "spring"),
    ("QuadrupedSmall", "feed"),
    ("QuadrupedSmall", "lateral"),
    ("Ship", "bone1_ori"),
    ("Ship", "bone2_ori"),
    ("Ship", "bone_rotation_rate"),
    ("Ship", "bone1_prop_trail_offset"),
    ("Ship", "bone2_prop_trail_offset"),
];

/// `(body kind, field)` groups whose catch-all is the *intended* value for
/// everything not explicitly listed, so a per-body ledger line would be noise.
///
/// The line is: does the wildcard mean **"this creature has no such feature"**,
/// or **"nobody said what this creature's number is"**? Only the first kind
/// belongs here.
///
/// - The twelve `BipedLarge` weapon-grip offsets position a *held weapon*
///   relative to the hand and control bones. The generic grip is deliberately
///   shared; only unusually-proportioned bodies override it, and the value
///   describes the weapon's mounting, not the creature.
/// - `BipedLarge.tail` -> `(0.0, 0.0)`, `Arthropod.snapper` -> `false`,
///   `QuadrupedLow.side_head_{lower,upper}` -> `(0,0,0)` (only Hydra has side
///   heads), `QuadrupedSmall.lateral` -> `0.0`. Each is a has-this-feature flag
///   whose off value is the whole point of the wildcard.
///
/// Everything else stays audited per body -- `scaler`, `tempo`, `spring`,
/// `startangle`, `feed`, `lean`, `maximize`/`minimize`, `leg_ori`, and both
/// `Object` and `Ship` bones -- because there the catch-all is a guess about
/// the creature, not a decision about a feature it lacks. `Object.bone0`/
/// `bone1` in particular stay audited on purpose: `Body::Object` is the
/// documented two-bone escape hatch for props and spell objects, so it is the
/// path a new asset is most likely to take.
const DEFAULTS_BY_DESIGN: &[(&str, &str)] = &[
    ("BipedLarge", "shl"),
    ("BipedLarge", "shr"),
    ("BipedLarge", "sc"),
    ("BipedLarge", "hhl"),
    ("BipedLarge", "hhr"),
    ("BipedLarge", "hc"),
    ("BipedLarge", "sthl"),
    ("BipedLarge", "sthr"),
    ("BipedLarge", "stc"),
    ("BipedLarge", "bhl"),
    ("BipedLarge", "bhr"),
    ("BipedLarge", "bc"),
    ("BipedLarge", "tail"),
    ("Arthropod", "snapper"),
    ("QuadrupedLow", "side_head_lower"),
    ("QuadrupedLow", "side_head_upper"),
    ("QuadrupedSmall", "lateral"),
];

/// True if `key` (`"<BodyKind>.<field> <body>"`) is in [`DEFAULTS_BY_DESIGN`].
fn defaults_by_design(key: &str) -> bool {
    let Some((prefix, _)) = key.split_once(' ') else {
        return false;
    };
    let Some((kind, field)) = prefix.split_once('.') else {
        return false;
    };
    DEFAULTS_BY_DESIGN.contains(&(kind, field))
}

/// Every entry in [`AUDITED_FIELDS`] must still be a real fall-through, so the
/// list cannot silently outlive the `attr_fallback!` it describes.
#[test]
fn audited_fields_are_all_live() {
    let observed = observed_fallbacks();
    for (kind, field) in AUDITED_FIELDS {
        let prefix = format!("{kind}.{field} ");
        assert!(
            observed.iter().any(|k| k.starts_with(&prefix)),
            "{kind}.{field} is listed in AUDITED_FIELDS but no body falls through to it any more \
             -- drop it from the list",
        );
    }
}

/// Every group in [`DEFAULTS_BY_DESIGN`] must still be a real fall-through, so
/// the exemption list cannot quietly outlive the wildcard it excuses.
#[test]
fn defaults_by_design_are_all_live() {
    let observed = observed_fallbacks();
    for (kind, field) in DEFAULTS_BY_DESIGN {
        let prefix = format!("{kind}.{field} ");
        assert!(
            observed.iter().any(|k| k.starts_with(&prefix)),
            "{kind}.{field} is listed in DEFAULTS_BY_DESIGN but no body falls through to it any \
             more — drop it from the list",
        );
    }
}

/// Rewrites the ledger from what the roster actually does.
///
/// Only runs under `ATTR_LEDGER_BLESS=1`; without it a stale ledger is a test
/// failure, which is the point.
fn bless(observed: &[String]) {
    use std::fmt::Write;

    let mut text = String::from(
        "# Ledger of `SkeletonAttr` fall-throughs -- generated, then reviewed by hand.\n#\n# Most \
         `SkeletonAttr` fields are exhaustive matches, so a new species will not\n# compile \
         without them. These are the minority that end in a wildcard: a new\n# species inherits \
         the catch-all, renders, and is quietly the wrong size with the\n# wrong gait. Every line \
         below is one (body kind, field, body) with no explicit\n# value today.\n#\n# Maintained \
         by `attr_audit_test.rs`. When the test fails it prints the exact\n# `+`/`-` lines; \
         `ATTR_LEDGER_BLESS=1 cargo test -p xindeler-anim --lib attr_audit`\n# regenerates this \
         file wholesale.\n#\n# Adding a species: give it an explicit arm. Fields whose catch-all \
         is a deliberate\n# feature-absence flag are exempted as a group in `DEFAULTS_BY_DESIGN` \
         and never\n# appear here.\n",
    );
    let mut group = String::new();
    for line in observed {
        let key = line.split(' ').next().unwrap_or_default();
        if key != group {
            group = key.to_owned();
            let _ = write!(text, "\n# --- {group} ---\n");
        }
        text.push_str(line);
        text.push('\n');
    }

    let path =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/attr_fallback_ledger.txt");
    std::fs::write(&path, text).expect("could not write the ledger");
    eprintln!("blessed {} ({} entries)", path.display(), observed.len());
}

#[test]
fn skeleton_attr_fallbacks_match_the_ledger() {
    let observed: Vec<String> = observed_fallbacks()
        .into_iter()
        .filter(|key| !defaults_by_design(key))
        .collect();

    if std::env::var_os("ATTR_LEDGER_BLESS").is_some() {
        bless(&observed);
        return;
    }

    let ledger: Vec<String> = include_str!("attr_fallback_ledger.txt")
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .map(str::to_owned)
        .collect();

    let added: Vec<&String> = observed.iter().filter(|h| !ledger.contains(h)).collect();
    let removed: Vec<&String> = ledger.iter().filter(|h| !observed.contains(h)).collect();

    if added.is_empty() && removed.is_empty() {
        return;
    }

    let mut msg = String::from(
        "\n`SkeletonAttr` fall-throughs no longer match \
         `voxygen/anim/src/attr_fallback_ledger.txt`.\n",
    );
    if !added.is_empty() {
        msg.push_str(
            "\nNEW silent defaults — these bodies have NO explicit value for the listed \
             animation\nattribute and are inheriting the body kind's catch-all. A wrong `scaler` \
             or `tempo`\nis a wrong-sized creature with a wrong gait and no diagnostic. Give each \
             an explicit\narm in the matching `voxygen/anim/src/<kind>/mod.rs`, or add the line \
             here with a\n`#` comment saying why the catch-all is right:\n",
        );
        for line in &added {
            msg.push_str("    + ");
            msg.push_str(line);
            msg.push('\n');
        }
    }
    if !removed.is_empty() {
        msg.push_str(
            "\nFIXED (or renamed/removed) — the ledger still lists these but they now have an \
             explicit\nvalue. Delete these lines from the ledger:\n",
        );
        for line in &removed {
            msg.push_str("    - ");
            msg.push_str(line);
            msg.push('\n');
        }
    }
    msg.push_str(
        "\nRe-bless with: ATTR_LEDGER_BLESS=1 cargo test -p xindeler-anim --lib attr_audit\n",
    );
    panic!("{msg}");
}

/// Guards the audit itself against silently becoming a no-op.
#[test]
fn the_audit_mechanism_is_wired_up() {
    // `Bonerattler` has no explicit `scaler` arm and must be seen falling
    // through; `Mammoth` has one and must not.
    let bonerattler = quadruped_medium::Body {
        species: quadruped_medium::Species::Bonerattler,
        body_type: quadruped_medium::BodyType::Male,
    };
    assert!(
        probe(|| crate::quadruped_medium::SkeletonAttr::from(&bonerattler))
            .contains(&("QuadrupedMedium", "scaler")),
        "the `attr_fallback!` recorder is not firing — the whole audit is dead"
    );
    let mammoth = quadruped_medium::Body {
        species: quadruped_medium::Species::Mammoth,
        body_type: quadruped_medium::BodyType::Male,
    };
    assert!(
        !probe(|| crate::quadruped_medium::SkeletonAttr::from(&mammoth))
            .contains(&("QuadrupedMedium", "scaler")),
    );
}
