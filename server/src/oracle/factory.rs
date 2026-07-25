//! Turns an [`EntityTemplate`]/[`DmEvent`] into real spawn data. Resolves
//! every free-form string field through the exact same vocabulary `/spawn`
//! already parses (`server/src/cmd.rs`) and follows the wildlife-spawn
//! pattern at `server/src/sys/terrain.rs:202-207`: build an `NpcBuilder`,
//! hand it (and a position/orientation) to the caller to emit as a
//! `CreateNpcEvent`. Unknown strings fall back to a safe default rather than
//! rejecting the template — "defuse, don't crash", same posture as the
//! schema's own `sanitize()`.

use common::{
    comp::{self, Alignment},
    event::NpcBuilder,
    lottery::LootSpec,
    npc,
};
use common_oracle::{AgentPreset, EntityTemplate};
use rand::{Rng, RngExt};
use vek::Vec3;

/// Resolves an [`EntityTemplate::faction`] string to an [`Alignment`].
/// `Alignment::Owned` is never produced here (it needs a runtime owner
/// entity, not expressible in a static template) — an unrecognized string,
/// same as an explicit `"wild"`, falls back to [`Alignment::Wild`].
fn resolve_alignment(faction: &str) -> Alignment {
    match faction {
        "enemy" => Alignment::Enemy,
        "npc" => Alignment::Npc,
        "tame" => Alignment::Tame,
        "passive" => Alignment::Passive,
        // "wild" and any unrecognized string both land here.
        _ => Alignment::Wild,
    }
}

/// Builds a real [`comp::Agent`] tuned to `preset`, seeded from `body`'s own
/// species-specific defaults and patrolling around `pos`.
fn build_agent(preset: AgentPreset, body: &comp::Body, pos: Vec3<f32>) -> comp::Agent {
    let mut agent = comp::Agent::from_body(body).with_patrol_origin(pos);
    match preset {
        AgentPreset::Passive => {
            agent.psyche.aggro_range_multiplier = 0.0;
            agent.psyche.flee_health = 0.0;
        },
        AgentPreset::Stalk => {},
        AgentPreset::Aggro => {
            agent = agent.with_aggro_no_warn();
            agent.psyche.aggro_range_multiplier = 2.0;
            agent.psyche.flee_health = 0.0;
        },
        AgentPreset::Flee => {
            agent.psyche.flee_health = 1.0;
        },
    }
    agent
}

/// Resolves `template` into a spawn-ready `(position, orientation,
/// NpcBuilder)` at `pos`. An unrecognized `template.body` falls back to
/// `"pig"` (never rejects the template).
pub fn spawn_entity_template(
    template: &EntityTemplate,
    pos: Vec3<f32>,
    rng: &mut impl Rng,
) -> (comp::Pos, comp::Ori, NpcBuilder) {
    let npc::NpcBody(npc_kind, mut make_body) = template
        .body
        .parse()
        .unwrap_or_else(|()| "pig".parse().expect("'pig' is a valid NpcBody"));
    let body = make_body();

    let name = template
        .stats
        .name
        .clone()
        .unwrap_or_else(|| npc::get_npc_name(npc_kind, npc::BodyType::from_body(body)));
    let stats = comp::Stats::new(comp::Content::Plain(name), body);
    let alignment = resolve_alignment(&template.faction);
    let agent = build_agent(
        AgentPreset::resolve(&template.ai_behavior_override),
        &body,
        pos,
    );
    // `EntityTemplate::loot` is a single item asset specifier (the shipped
    // sample templates under `assets/common/oracle/entity_templates/` each
    // name one specific item), not a loot table.
    let loot = template
        .loot
        .clone()
        .map_or(LootSpec::Nothing, LootSpec::Item);

    let npc_builder = NpcBuilder::new(stats, body, alignment)
        .with_agent(agent)
        .with_loot(loot);

    let ori = comp::Ori::from(common::util::Dir::random_2d(rng));
    (comp::Pos(pos), ori, npc_builder)
}

/// Resolves every `spawning_rules.entity_templates` id in `dm_event` against
/// `templates`, scattering `spawn_count` (rounded down) spawns within
/// `spawn_radius` of `origin`. Ids with no matching loaded template are
/// skipped (logged by the caller if desired) rather than aborting the whole
/// batch.
pub fn spawn_dm_event<'a>(
    spawning_rules: &common_oracle::SpawningRules,
    templates: impl Iterator<Item = &'a EntityTemplate> + Clone,
    origin: Vec3<f32>,
    rng: &mut impl Rng,
) -> Vec<(comp::Pos, comp::Ori, NpcBuilder)> {
    let matches: Vec<&EntityTemplate> = spawning_rules
        .entity_templates
        .iter()
        .filter_map(|id| templates.clone().find(|t| &t.entity_template_id == id))
        .collect();
    if matches.is_empty() {
        return Vec::new();
    }

    (0..spawning_rules.spawn_count as u32)
        .map(|i| {
            let template = matches[i as usize % matches.len()];
            let offset = Vec3::new(
                rng.random_range(-spawning_rules.spawn_radius..=spawning_rules.spawn_radius),
                rng.random_range(-spawning_rules.spawn_radius..=spawning_rules.spawn_radius),
                0.0,
            );
            spawn_entity_template(template, origin + offset, rng)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use common_oracle::SpawningRules;

    use super::*;

    #[test]
    fn resolve_alignment_falls_back_to_wild_for_unknown() {
        assert!(matches!(resolve_alignment("enemy"), Alignment::Enemy));
        assert!(matches!(resolve_alignment("npc"), Alignment::Npc));
        assert!(matches!(resolve_alignment("tame"), Alignment::Tame));
        assert!(matches!(resolve_alignment("passive"), Alignment::Passive));
        assert!(matches!(resolve_alignment("wild"), Alignment::Wild));
        assert!(matches!(
            resolve_alignment("definitely_not_a_real_faction"),
            Alignment::Wild
        ));
    }

    #[test]
    fn spawn_entity_template_resolves_a_known_body() {
        let template = EntityTemplate {
            entity_template_id: "dread_wolf".to_owned(),
            body: "wolf".to_owned(),
            faction: "enemy".to_owned(),
            ai_behavior_override: "aggro".to_owned(),
            ..Default::default()
        };
        let mut rng = rand::rng();
        let (pos, _ori, npc) = spawn_entity_template(&template, Vec3::new(1.0, 2.0, 3.0), &mut rng);
        assert_eq!(pos.0, Vec3::new(1.0, 2.0, 3.0));
        assert!(matches!(npc.alignment, Alignment::Enemy));
        assert!(matches!(npc.body, comp::Body::QuadrupedMedium(_)));
    }

    #[test]
    fn spawn_entity_template_falls_back_for_unknown_body() {
        let template = EntityTemplate {
            body: "definitely_not_a_real_body".to_owned(),
            ..Default::default()
        };
        let mut rng = rand::rng();
        // Must not panic; falls back to the "pig" body.
        let (_, _, npc) = spawn_entity_template(&template, Vec3::zero(), &mut rng);
        assert!(matches!(npc.body, comp::Body::QuadrupedSmall(_)));
    }

    #[test]
    fn spawn_dm_event_scatters_spawn_count_entities() {
        let template = EntityTemplate {
            entity_template_id: "dread_wolf".to_owned(),
            body: "wolf".to_owned(),
            ..Default::default()
        };
        let rules = SpawningRules {
            entity_templates: vec!["dread_wolf".to_owned()],
            spawn_count: 5.0,
            spawn_radius: 10.0,
            ..Default::default()
        };
        let mut rng = rand::rng();
        let spawns = spawn_dm_event(&rules, std::iter::once(&template), Vec3::zero(), &mut rng);
        assert_eq!(spawns.len(), 5);
    }

    #[test]
    fn spawn_dm_event_skips_unmatched_template_ids() {
        let rules = SpawningRules {
            entity_templates: vec!["no_such_template".to_owned()],
            spawn_count: 3.0,
            ..Default::default()
        };
        let mut rng = rand::rng();
        let spawns = spawn_dm_event(&rules, std::iter::empty(), Vec3::zero(), &mut rng);
        assert!(spawns.is_empty());
    }
}
