//! Deriving a summonable creature's Cadena point-pool cost from its combat
//! rating -- the same rating the HUD already floats over every creature's
//! head (`DerivedStats::compute`'s `combat_rating` field) -- rather than
//! hand-authoring a second difficulty axis that would drift from it on every
//! future stat/loadout rebalance.

use super::SummonTuning;
use crate::{
    comp::{
        Body, DerivedStats, Energy, Health, Inventory, Poise,
        inventory::{item::MaterialStatManifest, loadout_builder::LoadoutBuilder},
    },
    skillset_builder::SkillSetBuilder,
    states::basic_summon::SummonInfo,
};
use hashbrown::HashMap;
use std::sync::{OnceLock, RwLock};

/// A creature's cost against the Cadena point pool, derived from its combat
/// rating. Deliberately never hand-authored per creature.
pub fn summon_cost(combat_rating: f32, tuning: &SummonTuning) -> u16 {
    (combat_rating * tuning.cost_multiplier)
        .round()
        .max(tuning.cost_floor) as u16
}

/// The cost of the creature a `SummonInfo::Npc` would spawn. Constructs the
/// same throwaway `Inventory`/`SkillSet`/`Energy`/`Poise` that
/// `states::basic_summon`'s `CharacterBehavior::update` constructs when it
/// actually spawns one (`Energy`/`Poise` themselves are actually granted a
/// tick later, by `NpcBuilder`/`StateExt::create_npc` -- both depend only on
/// `Body`, so the values are identical either way) -- if that construction
/// ever changes, this must change with it (comment-tagged at both sites).
/// `has_health: false` means the real spawn gets no `Health` component at
/// all (a purely visual/projectile-styled summon), which this mirrors by
/// passing `None` for `health_base_max` -- `DerivedStats::compute` then
/// can't derive a combat rating and this returns the cost floor, not a
/// health-based one.
///
/// `SummonInfo` variants other than `Npc` (`BeamPillar`, `BeamWall`, ...)
/// have no creature and so no combat rating; they cost `0`. The Cadena boon
/// only ever grants `Npc` summons.
///
/// Expensive: constructs a throwaway `Inventory` + `SkillSet` and reads
/// `MaterialStatManifest`. Callers MUST cache this per ability id once, at
/// manifest-load time -- never call it in a per-cast or per-tick path.
pub fn npc_summon_cost(
    summon_info: &SummonInfo,
    msm: &MaterialStatManifest,
    tuning: &SummonTuning,
) -> u16 {
    let SummonInfo::Npc {
        body,
        loadout_config,
        skillset_config,
        has_health,
        ..
    } = summon_info
    else {
        return 0;
    };

    // Mirrors `states::basic_summon::CharacterBehavior::update`'s
    // `SummonInfo::Npc` arm exactly.
    let loadout = {
        let builder = LoadoutBuilder::empty().with_default_maintool(body);
        match loadout_config {
            Some(preset) => builder.with_preset(*preset).build(),
            None => builder.with_default_equipment(body).build(),
        }
    };
    let inventory = Inventory::with_loadout(loadout, *body);

    let skill_set = {
        let builder = SkillSetBuilder::default();
        match skillset_config {
            Some(preset) => builder.with_preset(*preset).build(),
            None => builder.build(),
        }
    };

    let health_base_max = has_health.then(|| Health::new(*body).base_max());
    let energy = Energy::new(*body);
    let poise = Poise::new(*body);

    let derived = DerivedStats::compute(
        Some(&inventory),
        None,
        Some(&skill_set),
        Some(*body),
        health_base_max,
        Some(energy.base_max()),
        Some(poise.base_max()),
        msm,
    );

    summon_cost(derived.combat_rating, tuning)
}

/// Process-lifetime cache backing [`cached_npc_summon_cost`], keyed by
/// [`Body`] alone: every field `npc_summon_cost` reads other than `body`
/// (`loadout_config`, `skillset_config`, `has_health`) is `None`/`true` on
/// every shipped Cadena RON (N27-O), so `Body` already uniquely determines
/// the cost for this boon's fixed roster. If a future Cadena ability ever
/// varies loadout/skillset per body, widen the key to
/// `(Body, Option<Preset>, Option<Preset>, bool)` instead of adding a
/// second cache.
fn summon_cost_cache() -> &'static RwLock<HashMap<Body, u16>> {
    static CACHE: OnceLock<RwLock<HashMap<Body, u16>>> = OnceLock::new();
    CACHE.get_or_init(|| RwLock::new(HashMap::new()))
}

/// [`npc_summon_cost`], computed once per [`Body`] and memoized for the rest
/// of the process's life. `npc_summon_cost` itself constructs a throwaway
/// `Inventory` + `SkillSet` and reads `MaterialStatManifest` -- a real
/// allocation this must never repeat on every cast or every per-creature
/// spawn in a batch. Both the client-side activation gate
/// (`CharacterAbility::requirements_paid`'s `BasicSummon` arm) and the
/// server's per-spawn authority gate
/// (`server::events::entity_creation::handle_create_npc`) call this instead
/// of `npc_summon_cost` directly, so a Cadena summon's cost is identical on
/// both sides of the gate by construction -- never recomputed independently.
pub fn cached_npc_summon_cost(
    summon_info: &SummonInfo,
    msm: &MaterialStatManifest,
    tuning: &SummonTuning,
) -> u16 {
    let SummonInfo::Npc { body, .. } = summon_info else {
        return 0;
    };
    if let Some(cost) = summon_cost_cache()
        .read()
        .expect("summon cost cache poisoned")
        .get(body)
    {
        return *cost;
    }
    let cost = npc_summon_cost(summon_info, msm, tuning);
    summon_cost_cache()
        .write()
        .expect("summon cost cache poisoned")
        .insert(*body, cost);
    cost
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::comp::{Body, body};
    use rand::{SeedableRng, rngs::SmallRng};

    fn tuning() -> SummonTuning { super::super::summon_tuning_manifest().0 }

    fn npc_summon_info(body: Body, has_health: bool) -> SummonInfo {
        SummonInfo::Npc {
            summoned_amount: 1,
            summon_distance: (1.0, 1.0),
            body,
            scale: None,
            has_health,
            use_npc_name: false,
            loadout_config: None,
            skillset_config: None,
            duration: None,
            alignment: None,
            with_agent: true,
            incorporeal: false,
            phantom_illusion: false,
            delete_after_expiry: false,
            pact_chain_summon: false,
        }
    }

    fn cost_of(body: Body) -> u16 {
        let msm = MaterialStatManifest::load().cloned();
        npc_summon_cost(&npc_summon_info(body, true), &msm, &tuning())
    }

    fn biped_small(species: body::biped_small::Species) -> Body {
        let mut rng = SmallRng::seed_from_u64(0);
        Body::BipedSmall(body::biped_small::Body::random_with(&mut rng, &species))
    }

    fn biped_large(species: body::biped_large::Species) -> Body {
        let mut rng = SmallRng::seed_from_u64(0);
        Body::BipedLarge(body::biped_large::Body::random_with(&mut rng, &species))
    }

    #[test]
    fn zero_combat_rating_still_floors_to_a_cost_of_one() {
        // A degenerate/zero rating must never be free to summon.
        assert_eq!(summon_cost(0.0, &tuning()), 1);
    }

    #[test]
    fn cost_scales_with_combat_rating() {
        assert_eq!(summon_cost(1.0, &tuning()), 2);
        assert_eq!(summon_cost(10.4, &tuning()), 21);
    }

    #[test]
    fn non_npc_summon_info_costs_nothing() {
        let msm = MaterialStatManifest::load().cloned();
        let pillar = SummonInfo::BeamPillar {
            buildup_duration: 0.5,
            attack_duration: 1.0,
            beam_duration: 1.0,
            target: crate::states::basic_summon::BeamPillarTarget::Single,
            radius: 1.0,
            height: 1.0,
            damage: 1.0,
            damage_effect: None,
            dodgeable: Default::default(),
            tick_rate: 1.0,
            specifier: crate::comp::beam::FrontendSpecifier::Steam,
            indicator_specifier:
                crate::states::basic_summon::BeamPillarIndicatorSpecifier::FirePillar,
        };
        assert_eq!(npc_summon_cost(&pillar, &msm, &tuning()), 0);
    }

    #[test]
    fn a_health_less_summon_gets_the_cost_floor_not_a_health_based_one() {
        // `has_health: false` means the real spawn never gets a `Health`
        // component (a purely visual/projectile-styled summon) -- mirrored
        // here as `health_base_max: None`, which `DerivedStats::compute`
        // can't turn into a combat rating, so this must land on the floor
        // rather than silently pricing it as if it had a body's full HP.
        let msm = MaterialStatManifest::load().cloned();
        let healthy = npc_summon_cost(
            &npc_summon_info(
                Body::BipedSmall(body::biped_small::Body::random_with(
                    &mut SmallRng::seed_from_u64(0),
                    &body::biped_small::Species::Husk,
                )),
                true,
            ),
            &msm,
            &tuning(),
        );
        let healthless = npc_summon_cost(
            &npc_summon_info(
                Body::BipedSmall(body::biped_small::Body::random_with(
                    &mut SmallRng::seed_from_u64(0),
                    &body::biped_small::Species::Husk,
                )),
                false,
            ),
            &msm,
            &tuning(),
        );
        assert_eq!(healthless, tuning().cost_floor as u16);
        assert!(healthless <= healthy);
    }

    /// Regression test pinning the *ordering* the plan derives from the
    /// shipped roster, not the exact float -- the real numbers are computed
    /// at load and will drift with any future stat/loadout rebalance, which
    /// is the entire point of deriving rather than hand-authoring.
    #[test]
    fn cost_ordering_matches_the_shipped_roster() {
        let husk = cost_of(biped_small(body::biped_small::Species::Husk));
        let blueoni = cost_of(biped_large(body::biped_large::Species::Blueoni));
        let dullahan = cost_of(biped_large(body::biped_large::Species::Dullahan));
        let harvester = cost_of(biped_large(body::biped_large::Species::Harvester));
        let mindflayer = cost_of(biped_large(body::biped_large::Species::Mindflayer));

        // Pins today's actual computed ordering (verified against the live
        // `combat_rating` formula, not the plan's own approximate worked
        // table -- that table explicitly caveats "the point is the
        // ordering, not the third decimal", and the real per-species
        // default-loadout stats put Dullahan above Harvester).
        assert!(husk < blueoni, "{husk} !< {blueoni}");
        assert!(blueoni < harvester, "{blueoni} !< {harvester}");
        assert!(harvester < dullahan, "{harvester} !< {dullahan}");
        assert!(dullahan < mindflayer, "{dullahan} !< {mindflayer}");
        assert!(
            mindflayer > 25,
            "a Mindflayer ({mindflayer}) must sit past the 25-point pool ceiling"
        );
    }

    #[test]
    fn cached_cost_matches_the_uncached_derivation() {
        let msm = MaterialStatManifest::load().cloned();
        let info = npc_summon_info(biped_small(body::biped_small::Species::Husk), true);
        let uncached = npc_summon_cost(&info, &msm, &tuning());
        let cached_first_call = cached_npc_summon_cost(&info, &msm, &tuning());
        let cached_second_call = cached_npc_summon_cost(&info, &msm, &tuning());
        assert_eq!(uncached, cached_first_call);
        assert_eq!(cached_first_call, cached_second_call);
    }

    #[test]
    fn cached_cost_of_a_non_npc_summon_is_zero_and_uncached() {
        let msm = MaterialStatManifest::load().cloned();
        let pillar = SummonInfo::BeamPillar {
            buildup_duration: 0.5,
            attack_duration: 1.0,
            beam_duration: 1.0,
            target: crate::states::basic_summon::BeamPillarTarget::Single,
            radius: 1.0,
            height: 1.0,
            damage: 1.0,
            damage_effect: None,
            dodgeable: Default::default(),
            tick_rate: 1.0,
            specifier: crate::comp::beam::FrontendSpecifier::Steam,
            indicator_specifier:
                crate::states::basic_summon::BeamPillarIndicatorSpecifier::FirePillar,
        };
        assert_eq!(cached_npc_summon_cost(&pillar, &msm, &tuning()), 0);
    }

    #[test]
    fn a_trash_tier_creature_is_affordable_at_the_starting_pool() {
        // The level-1 grant is 2 points; a Husk-tier fiend must fit it (plan
        // §14.1d's "one Husk-tier fiend" grant).
        let husk = cost_of(biped_small(body::biped_small::Species::Husk));
        assert!(husk <= 2, "Husk cost {husk} exceeds the level-1 pool of 2");
    }
}
