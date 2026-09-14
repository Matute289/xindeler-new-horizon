//! Roster-wide audit of per-species attribute fall-throughs.
//!
//! See [`super::attr_audit`] for the mechanism. This test walks **every**
//! species of every body kind, calls each audited attribute getter, and
//! records the ones that fell through to a wildcard instead of having an
//! explicit value.
//!
//! The result is diffed against the checked-in ledger
//! `attr_fallback_ledger.txt`. A new species that forgets an explicit value
//! therefore fails this test by name; a species that *gains* an explicit value
//! also fails, so the ledger can never drift into fiction.
//!
//! Re-bless the ledger after a deliberate change with
//! `ATTR_LEDGER_BLESS=1 cargo test -p xindeler-common --lib attr_audit`.
//!
//! **Scope.** Only the getters in [`AUDITED`] are covered — the per-species
//! attributes that feed balance, physics or AI targeting. Attributes whose
//! catch-all *is* the meaning (`immune_to`, `negates_buff`,
//! `is_same_species_as`, `localize_npc`, `humanoid_gender`) are deliberately
//! out of scope, as are `dimensions`/`base_health`-style matches that are
//! already exhaustive and enforced by the compiler.

use std::collections::HashMap;

use super::{
    Body, arthropod, attr_audit::probe, biped_large, biped_small, bird_large, bird_medium,
    crustacean, dragon, fish_medium, fish_small, golem, humanoid, object, quadruped_low,
    quadruped_medium, quadruped_small, ship, theropod,
};

/// The audited getters, by the name used in the ledger.
///
/// Every entry is a per-species attribute that feeds game balance, physics or
/// AI targeting — the class where a silent default produces a creature that is
/// quietly wrong rather than merely unstyled.
const AUDITED: &[(&str, fn(&Body))] = &[
    ("scale", |b| {
        b.scale();
    }),
    ("mass", |b| {
        b.mass();
    }),
    ("spacing_radius", |b| {
        b.spacing_radius();
    }),
    ("base_energy", |b| {
        b.base_energy();
    }),
    ("threat_tier", |b| {
        b.threat_tier();
    }),
    ("base_health", |b| {
        b.base_health();
    }),
    ("combat_multiplier", |b| {
        b.combat_multiplier();
    }),
    ("magic_resist_tier", |b| {
        b.magic_resist_tier();
    }),
    ("base_poise", |b| {
        b.base_poise();
    }),
];

/// The ledger label for a `Body`'s kind.
///
/// Deliberately an **exhaustive** match: adding a `Body` variant stops this
/// file compiling, which is the only way to be sure the roster below did not
/// quietly lose a whole body kind and start passing vacuously.
fn body_kind(body: &Body) -> &'static str {
    match body {
        Body::Humanoid(_) => "Humanoid",
        Body::QuadrupedSmall(_) => "QuadrupedSmall",
        Body::QuadrupedMedium(_) => "QuadrupedMedium",
        Body::BirdMedium(_) => "BirdMedium",
        Body::FishMedium(_) => "FishMedium",
        Body::Dragon(_) => "Dragon",
        Body::BirdLarge(_) => "BirdLarge",
        Body::FishSmall(_) => "FishSmall",
        Body::BipedLarge(_) => "BipedLarge",
        Body::BipedSmall(_) => "BipedSmall",
        Body::Object(_) => "Object",
        Body::Golem(_) => "Golem",
        Body::Theropod(_) => "Theropod",
        Body::QuadrupedLow(_) => "QuadrupedLow",
        Body::Ship(_) => "Ship",
        Body::Arthropod(_) => "Arthropod",
        Body::Item(_) => "Item",
        Body::Crustacean(_) => "Crustacean",
        Body::Plugin(_) => "Plugin",
    }
}

/// Every body kind [`roster`] is expected to produce at least one body for.
///
/// `Item` and `Plugin` are the two exceptions — see [`roster`] for why.
const EXPECTED_KINDS: &[&str] = &[
    "Arthropod",
    "BipedLarge",
    "BipedSmall",
    "BirdLarge",
    "BirdMedium",
    "Crustacean",
    "Dragon",
    "FishMedium",
    "FishSmall",
    "Golem",
    "Humanoid",
    "Object",
    "QuadrupedLow",
    "QuadrupedMedium",
    "QuadrupedSmall",
    "Ship",
    "Theropod",
];

/// Every `Body` in the game, paired with the ledger label for its species.
///
/// The label deliberately omits the body type: no audited attribute varies by
/// it today, and [`observed_fallbacks`] asserts that stays true, so a ledger
/// keyed on species alone is both half the size and unambiguous.
///
/// Two body kinds are absent, both deliberately:
///
/// - `Body::Item` — its variants carry payloads (`Tool(ToolKind)`,
///   `Armor(ItemArmorKind)`) so there is no flat list to walk, and no audited
///   getter has a per-variant match for it; every one treats `Item` as a single
///   case. Add it here the moment that stops being true.
/// - `Body::Plugin` — `plugin::Body::mass()` (and its siblings) index straight
///   into the `PLUGIN_SPECIES` registry, which is empty unless plugins are
///   loaded, so constructing one in a unit test panics with an out-of-bounds
///   index. That is a pre-existing sharp edge in
///   `common/src/comp/body/plugin.rs`, not something this audit should paper
///   over; plugin bodies also carry their own per-species data rather than a
///   match arm, so there is no wildcard to audit.
fn roster() -> Vec<(String, Body)> {
    let mut out = Vec::new();

    macro_rules! species_body {
        ($module:ident) => {
            for species in $module::ALL_SPECIES {
                for body_type in $module::ALL_BODY_TYPES {
                    out.push((
                        format!("{species:?}"),
                        Body::from($module::Body { species, body_type }),
                    ));
                }
            }
        };
    }

    species_body!(arthropod);
    species_body!(biped_large);
    species_body!(biped_small);
    species_body!(bird_large);
    species_body!(bird_medium);
    species_body!(crustacean);
    species_body!(dragon);
    species_body!(fish_medium);
    species_body!(fish_small);
    species_body!(golem);
    species_body!(quadruped_low);
    species_body!(quadruped_medium);
    species_body!(quadruped_small);
    species_body!(theropod);

    for body in humanoid::Body::iter() {
        out.push((format!("{:?}", body.species), Body::Humanoid(body)));
    }
    for obj in object::ALL_OBJECTS {
        out.push((format!("{obj:?}"), Body::Object(obj)));
    }
    for hull in ship::ALL_BODIES {
        out.push((format!("{hull:?}"), Body::Ship(hull)));
    }
    out
}

/// Collects `"<BodyKind>.<attr> <species>"` for every fall-through that fires.
fn observed_fallbacks() -> Vec<String> {
    // key -> (times the fall-through fired, times the key was tried)
    let mut hits: HashMap<String, (u32, u32)> = HashMap::new();
    for (label, body) in roster() {
        let kind = body_kind(&body);
        for (attr, call) in AUDITED {
            let taken = probe(|| call(&body));
            for (recorded_kind, recorded_attr) in &taken {
                // The recorded attribute must be the one we asked for and the
                // recorded body kind must be the one we called with (or the
                // `"*"` used for an outer, kind-spanning wildcard); otherwise
                // a wrapped wildcard carries a copy-pasted label.
                assert_eq!(
                    *recorded_attr, *attr,
                    "an `attr_fallback!` labelled {recorded_attr:?} fired while evaluating \
                     {attr:?} on a {kind} — fix the label",
                );
                assert!(
                    *recorded_kind == kind || *recorded_kind == "*",
                    "the `attr_fallback!` for {attr} that fired on a {kind} is labelled \
                     {recorded_kind:?} — fix the label",
                );
            }
            let entry = hits.entry(format!("{kind}.{attr} {label}")).or_default();
            entry.1 += 1;
            if !taken.is_empty() {
                entry.0 += 1;
            }
        }
    }

    let mut out = Vec::new();
    for (key, (fired, tried)) in hits {
        assert!(
            fired == 0 || fired == tried,
            "{key}: the fall-through fired for {fired} of {tried} body types. An audited \
             attribute that varies by body type breaks this ledger's species-only key — either \
             give every body type an explicit value or extend the ledger key to include it.",
        );
        if fired > 0 {
            out.push(key);
        }
    }
    out.sort();
    out
}

/// Audited attributes whose catch-all is the *neutral* value rather than a
/// guess about the creature, so a per-species ledger line would carry no
/// information.
///
/// The line is: does the wildcard mean **"this creature has no such trait"**,
/// or **"nobody said what this creature's number is"**? The four below are the
/// first kind — each of their matches is deliberately sparse, and the source
/// says so (`magic_resist_tier`: *"A sparse match; anything not listed has no
/// innate resistance"*):
///
/// - `scale` — `1.0` is "no scaling"; body size lives in `dimensions()`.
/// - `spacing_radius` — `2.0` is the standard AI spacing; only Rat, Hakulaq and
///   Husk deviate.
/// - `magic_resist_tier` — `None` is "no innate magic resistance".
/// - `combat_multiplier` — `1.0` is "no difficulty adjustment".
///
/// Everything left audited (`mass`, `base_health`, `base_poise`,
/// `base_energy`, `threat_tier`) is a substantive number about the creature,
/// where a catch-all really is somebody's forgotten decision. They stay
/// wrapped in `attr_fallback!` either way, so flipping one of these back to
/// audited is a one-line change.
const DEFAULTS_BY_DESIGN: &[&str] = &[
    "scale",
    "spacing_radius",
    "magic_resist_tier",
    "combat_multiplier",
];

/// True if `key` (`"<BodyKind>.<attr> <species>"`) names a
/// [`DEFAULTS_BY_DESIGN`] attribute.
fn defaults_by_design(key: &str) -> bool {
    key.split(' ')
        .next()
        .and_then(|prefix| prefix.split_once('.'))
        .is_some_and(|(_, attr)| DEFAULTS_BY_DESIGN.contains(&attr))
}

/// Every attribute in [`DEFAULTS_BY_DESIGN`] must still be a live
/// fall-through, so the exemption list cannot outlive the wildcard it excuses.
#[test]
fn defaults_by_design_are_all_live() {
    let observed = observed_fallbacks();
    for attr in DEFAULTS_BY_DESIGN {
        assert!(
            observed.iter().any(|key| key
                .split(' ')
                .next()
                .and_then(|prefix| prefix.split_once('.'))
                .is_some_and(|(_, a)| a == *attr)),
            "{attr} is listed in DEFAULTS_BY_DESIGN but nothing falls through to it any more —              drop it from the list",
        );
    }
}

const LEDGER_PATH: &str = "common/src/comp/body/attr_fallback_ledger.txt";

fn ledger() -> Vec<String> {
    include_str!("attr_fallback_ledger.txt")
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .map(str::to_owned)
        .collect()
}

/// Rewrites the ledger from what the roster actually does.
///
/// Only runs under `ATTR_LEDGER_BLESS=1`; without it a stale ledger is a test
/// failure, which is the point.
fn bless(observed: &[String]) {
    use std::fmt::Write;

    let mut text = String::from(
        "# Ledger of per-species attribute fall-throughs — generated, then reviewed by \
         hand.\n#\n# Every line is a (body kind, attribute, species) that has NO explicit value \
         in\n# `common/src/comp/body/mod.rs` and is quietly inheriting a catch-all arm. Each \
         line\n# is technical debt, not an endorsement: it is here so the debt is countable, and \
         so\n# that a *newly added* species that forgets a value fails\n# `cargo test -p \
         xindeler-common` instead of shipping silently wrong.\n#\n# Maintained by \
         `attr_audit_test.rs`. When the test fails it prints the exact `+`/`-`\n# lines to add or \
         delete; `ATTR_LEDGER_BLESS=1 cargo test -p xindeler-common --lib\n# attr_audit` \
         regenerates this file wholesale.\n#\n# Adding a species: give it an explicit arm. Only \
         let a line appear here if the\n# catch-all genuinely is the right value for it.\n# \
         Fixing a species: give it an explicit arm; the line disappears on the next bless.\n#\n# \
         SCOPE: this covers the getters in `AUDITED` minus those in `DEFAULTS_BY_DESIGN`\n# \
         (attributes whose catch-all is the neutral value, not a guess). It is not a\n# complete \
         inventory of every default in the engine — see the module docs.\n",
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

    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("src/comp/body/attr_fallback_ledger.txt");
    std::fs::write(&path, text).expect("could not write the ledger");
    eprintln!("blessed {} ({} entries)", path.display(), observed.len());
}

#[test]
fn species_attribute_fallbacks_match_the_ledger() {
    let observed: Vec<String> = observed_fallbacks()
        .into_iter()
        .filter(|key| !defaults_by_design(key))
        .collect();

    if std::env::var_os("ATTR_LEDGER_BLESS").is_some() {
        bless(&observed);
        return;
    }

    let ledger = ledger();
    let added: Vec<&String> = observed.iter().filter(|h| !ledger.contains(h)).collect();
    let removed: Vec<&String> = ledger.iter().filter(|h| !observed.contains(h)).collect();

    if added.is_empty() && removed.is_empty() {
        return;
    }

    let mut msg = format!("\nper-species attribute fall-throughs no longer match {LEDGER_PATH}.\n");
    if !added.is_empty() {
        msg.push_str(
            "\nNEW silent defaults — these species have NO explicit value and are quietly \
             inheriting\na catch-all arm. Give each one an explicit arm in \
             `common/src/comp/body/mod.rs`\n(this is almost always what you want); only bless the \
             ledger if the catch-all really\nis correct for them:\n",
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
             explicit\nvalue:\n",
        );
        for line in &removed {
            msg.push_str("    - ");
            msg.push_str(line);
            msg.push('\n');
        }
    }
    msg.push_str(
        "\nRe-bless with: ATTR_LEDGER_BLESS=1 cargo test -p xindeler-common --lib attr_audit\n",
    );
    panic!("{msg}");
}

/// The roster must reach every body kind, or a wildcard wrapped in a kind that
/// is never constructed would pass vacuously for ever.
#[test]
fn the_roster_reaches_every_body_kind() {
    let reached: Vec<&'static str> = roster().iter().map(|(_, b)| body_kind(b)).collect();
    for kind in EXPECTED_KINDS {
        assert!(
            reached.contains(kind),
            "no {kind} body in the audit roster — any `attr_fallback!` in it would never fire",
        );
    }
}

/// Guards the audit itself: if every `attr_fallback!` were accidentally
/// removed or the recorder silently stopped working, the ledger diff above
/// would still pass on an empty roster. This asserts the mechanism is live.
#[test]
fn the_audit_mechanism_is_wired_up() {
    assert!(
        !roster().is_empty(),
        "the body roster is empty — the audit would vacuously pass"
    );
    // `Wolf` has no explicit `mass` arm and must be seen falling through.
    let wolf = Body::from(quadruped_medium::Body {
        species: quadruped_medium::Species::Wolf,
        body_type: quadruped_medium::BodyType::Male,
    });
    assert_eq!(
        probe(|| wolf.mass()),
        vec![("QuadrupedMedium", "mass")],
        "the `attr_fallback!` recorder is not firing — the whole audit is dead"
    );
    // …and a species that *does* have one must not be reported.
    let bear = Body::from(quadruped_medium::Body {
        species: quadruped_medium::Species::Bear,
        body_type: quadruped_medium::BodyType::Male,
    });
    assert!(probe(|| bear.mass()).is_empty());
}
