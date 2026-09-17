//! TEMPORARY migration proof — asserts `assets/common/body_stats.ron` returns
//! byte-identical numbers to the `match` arms it is about to replace, for
//! every species of every body kind.
//!
//! Deleted in the commit that rewires the getters, at which point it would be
//! comparing the table with itself.

use xindeler_common::comp::{
    Body, arthropod, biped_large, biped_small, bird_large, bird_medium, body::stats::body_stats,
    crustacean, dragon, fish_medium, fish_small, golem, quadruped_low, quadruped_medium,
    quadruped_small, theropod,
};

macro_rules! check {
    ($stats:ident, $module:ident) => {
        for species in $module::ALL_SPECIES {
            let body = Body::from($module::Body {
                species,
                body_type: $module::ALL_BODY_TYPES[0],
            });
            let row = body.stats_row(&$stats.0).ok().expect("has a row");
            assert_eq!(row.mass, body.mass().0, "{species:?} mass");
            assert_eq!(row.base_health, body.base_health(), "{species:?} health");
            assert_eq!(row.base_poise, body.base_poise(), "{species:?} poise");
            assert_eq!(row.base_energy, body.base_energy(), "{species:?} energy");
        }
    };
}

#[test]
fn ron_matches_code() {
    let stats = body_stats();
    check!(stats, arthropod);
    check!(stats, biped_large);
    check!(stats, biped_small);
    check!(stats, bird_large);
    check!(stats, bird_medium);
    check!(stats, crustacean);
    check!(stats, dragon);
    check!(stats, fish_medium);
    check!(stats, fish_small);
    check!(stats, golem);
    check!(stats, quadruped_low);
    check!(stats, quadruped_medium);
    check!(stats, quadruped_small);
    check!(stats, theropod);
}
