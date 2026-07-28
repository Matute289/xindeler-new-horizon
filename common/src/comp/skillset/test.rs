use super::*;

// Unneeded cfg(test) here keeps rust-analyzer happy
#[cfg(test)]
use petgraph::{algo::is_cyclic_directed, graph::DiGraph};

#[test]
fn check_cyclic_skill_deps() {
    let skill_prereqs: HashMap<Skill, SkillPrerequisite> =
        Ron::load_expect_cloned("common.skill_trees.skill_prerequisites").0;
    let mut graph = DiGraph::new();
    let mut nodes = HashMap::<Skill, _>::new();
    let mut add_node = |graph: &mut DiGraph<Skill, _>, node: Skill| {
        *nodes.entry(node).or_insert_with(|| graph.add_node(node))
    };

    for (skill, prereqs) in skill_prereqs.iter() {
        let skill_node = add_node(&mut graph, *skill);
        let prereqs = match prereqs {
            SkillPrerequisite::Any(skills) => skills,
            SkillPrerequisite::All(skills) => skills,
        };
        for (prereq, _) in prereqs.iter() {
            let prereq_node = add_node(&mut graph, *prereq);
            graph.add_edge(prereq_node, skill_node, ());
        }
    }

    assert!(!is_cyclic_directed(&graph));
}

// ---- BL-06 class skill trees ----

#[test]
fn class_skill_persistence_round_trip() {
    use crate::comp::skills::{MageSkill, WarriorSkill};
    // Class skills persist via serde (json) like weapon skills; a new variant
    // must round-trip without a manual conversion arm.
    let skills = vec![
        Skill::Warrior(WarriorSkill::Onslaught),
        Skill::Mage(MageSkill::FocusedMind),
        Skill::Warrior(WarriorSkill::HardenedBody),
    ];
    let json = serde_json::to_string(&skills).expect("serialize class skills");
    let back: Vec<Skill> = serde_json::from_str(&json).expect("deserialize class skills");
    assert_eq!(skills, back);
}

#[test]
fn class_passive_raises_stats_field() {
    use crate::comp::{Body, Stats, body::humanoid, skills::WarriorSkill};

    let body = Body::Humanoid(humanoid::Body::iter().next().expect("a humanoid body"));
    let mut skillset = SkillSet::default();
    // Seed a leveled passive directly (the unlock flow is covered elsewhere).
    skillset
        .skills
        .insert(Skill::Warrior(WarriorSkill::HardenedBody), 2);

    let mut stats = Stats::empty(body);
    let before = stats.max_health_modifiers.mult_mod;
    skillset.apply_class_passives(&mut stats);
    // HardenedBody = +0.04 max-health per level; level 2 → *= 1.08.
    assert!((stats.max_health_modifiers.mult_mod - before * 1.08).abs() < 1e-5);
}

#[test]
fn caster_passives_use_spell_power_channel() {
    use crate::comp::{Body, Stats, body::humanoid, skills::MageSkill};

    let body = Body::Humanoid(humanoid::Body::iter().next().expect("a humanoid body"));
    let mut skillset = SkillSet::default();
    // SpellPotency is re-pointed to the magic-only `spell_power` channel (Q2/Q3)
    // so it must NOT touch the global physical `attack_damage_modifier`.
    skillset
        .skills
        .insert(Skill::Mage(MageSkill::SpellPotency), 3);

    let mut stats = Stats::empty(body);
    let attack_before = stats.attack_damage_modifier;
    skillset.apply_class_passives(&mut stats);
    // SpellPotency = +0.04 spell_power per level; level 3 → *= 1.12.
    assert!((stats.spell_power - 1.12).abs() < 1e-5);
    assert_eq!(
        stats.attack_damage_modifier, attack_before,
        "caster damage passive must not leak onto physical attack_damage_modifier",
    );
}

#[test]
fn heal_power_passive_applies() {
    use crate::comp::{Stats, body::humanoid, skills::ClassPassiveStat};

    let body = crate::comp::Body::Humanoid(humanoid::Body::iter().next().unwrap());
    let mut stats = Stats::empty(body);
    ClassPassiveStat::HealPower.apply(&mut stats, 0.2);
    assert!((stats.heal_power - 1.2).abs() < 1e-5);
}

#[test]
fn undead_body_tag_and_smite_passive() {
    use crate::comp::{
        Body, CreatureKind, Stats,
        body::{biped_small, humanoid},
        skills::ClericSkill,
    };

    // Body::creature_kind (Q4): Undead for an undead species, not for a humanoid.
    let husk = Body::BipedSmall(biped_small::Body {
        species: biped_small::Species::Husk,
        body_type: biped_small::BodyType::Male,
    });
    assert_eq!(husk.creature_kind(), Some(CreatureKind::Undead));
    let human = Body::Humanoid(humanoid::Body::iter().next().unwrap());
    assert_ne!(human.creature_kind(), Some(CreatureKind::Undead));

    // SmitingStrikes folds into both spell_power and bonus_damage_vs_undead.
    let mut skillset = SkillSet::default();
    skillset
        .skills
        .insert(Skill::Cleric(ClericSkill::SmitingStrikes), 2);
    let mut stats = Stats::empty(human);
    skillset.apply_class_passives(&mut stats);
    assert!((stats.spell_power - 1.08).abs() < 1e-5); // +0.04 spell_power/level
    assert!((stats.bonus_damage_vs_undead - 0.20).abs() < 1e-5); // +0.10/level
}

#[test]
fn active_skills_have_no_passive_modifier() {
    use crate::comp::skills::{ClericSkill, MageSkill, RogueSkill, WarriorSkill};
    // The 8 signature/capstone actives unlock abilities, not passive stats —
    // they must be absent from the modifier manifest.
    for active in [
        Skill::Warrior(WarriorSkill::Rally),
        Skill::Warrior(WarriorSkill::Onslaught),
        Skill::Mage(MageSkill::ArcaneSurge),
        Skill::Mage(MageSkill::ArcaneMastery),
        Skill::Cleric(ClericSkill::MendingLight),
        Skill::Cleric(ClericSkill::RadiantChannel),
        Skill::Rogue(RogueSkill::Ambush),
        Skill::Rogue(RogueSkill::Vanish),
    ] {
        assert!(
            CLASS_SKILL_MODIFIERS.get(&active).is_none(),
            "{active:?} is an active ability and must not have a passive modifier",
        );
    }
}

#[test]
fn class_skill_modifiers_manifest_integrity() {
    // Every modifier entry must be a real skill living in a Class skill group.
    for skill in CLASS_SKILL_MODIFIERS.keys() {
        let group = SKILL_GROUP_LOOKUP
            .get(skill)
            .unwrap_or_else(|| panic!("{skill:?} has a modifier but is in no skill group"));
        assert!(
            matches!(group, SkillGroupKind::Class(_)),
            "{skill:?} modifier must belong to a Class group, got {group:?}",
        );
    }
}

// ---- BL-20 feats/skills system ----

#[test]
fn feat_skill_persistence_round_trip() {
    use crate::comp::skills::FeatSkill;
    // Feat skills persist via serde (json) like weapon/class skills; a new
    // variant must round-trip without a manual conversion arm.
    let skills = vec![
        Skill::Feat(FeatSkill::Tough),
        Skill::Feat(FeatSkill::Lucky),
        Skill::Feat(FeatSkill::GreaterAberrantBloodmark),
    ];
    let json = serde_json::to_string(&skills).expect("serialize feat skills");
    let back: Vec<Skill> = serde_json::from_str(&json).expect("deserialize feat skills");
    assert_eq!(skills, back);
}

#[test]
fn default_skillset_has_feats_group_unlocked() {
    let skill_set = SkillSet::default();
    assert!(
        skill_set.skill_group_accessible(SkillGroupKind::Feats),
        "a fresh SkillSet must have the Feats group unlocked from creation"
    );
}

#[test]
fn grant_skill_point_bypasses_exp_economy() {
    let mut skill_set = SkillSet::default();
    let earned_exp_before = skill_set.total_earned_exp();
    let available_exp_before = skill_set.available_experience(SkillGroupKind::Feats);

    skill_set.grant_skill_point(SkillGroupKind::Feats);

    assert_eq!(skill_set.available_sp(SkillGroupKind::Feats), 1);
    assert_eq!(skill_set.earned_sp(SkillGroupKind::Feats), 1);
    assert_eq!(
        skill_set.available_experience(SkillGroupKind::Feats),
        available_exp_before,
        "grant_skill_point must not touch available_exp"
    );
    assert_eq!(
        skill_set.total_earned_exp(),
        earned_exp_before,
        "grant_skill_point must not touch earned_exp"
    );
}

#[test]
fn grant_skill_point_unlocks_group_if_missing() {
    // Simulate an entity that hasn't had the Feats group unlocked yet (e.g. a
    // pre-BL-20 persisted character) — grant_skill_point should unlock it
    // first rather than silently no-op.
    let mut skill_set = SkillSet {
        skill_groups: HashMap::new(),
        skills: HashMap::new(),
        character_level: 0,
    };
    assert!(!skill_set.skill_group_accessible(SkillGroupKind::Feats));

    skill_set.grant_skill_point(SkillGroupKind::Feats);

    assert!(skill_set.skill_group_accessible(SkillGroupKind::Feats));
    assert_eq!(skill_set.available_sp(SkillGroupKind::Feats), 1);
    assert_eq!(skill_set.earned_sp(SkillGroupKind::Feats), 1);
}

#[test]
fn feat_modifiers_manifest_integrity() {
    // Every modifier entry must be a real skill living in the Feats skill
    // group (mirrors class_skill_modifiers_manifest_integrity).
    for skill in FEAT_MODIFIERS.keys() {
        let group = SKILL_GROUP_LOOKUP
            .get(skill)
            .unwrap_or_else(|| panic!("{skill:?} has a modifier but is in no skill group"));
        assert!(
            matches!(group, SkillGroupKind::Feats),
            "{skill:?} modifier must belong to the Feats group, got {group:?}",
        );
    }
}

#[test]
fn apply_feat_passives_raises_stats_field() {
    use crate::comp::{Body, Stats, body::humanoid, skills::FeatSkill};

    let body = Body::Humanoid(humanoid::Body::iter().next().expect("a humanoid body"));
    let mut skillset = SkillSet::default();
    // Seed a purchased passive feat directly (the unlock flow is covered
    // elsewhere). Feats are max_level = 1.
    skillset.skills.insert(Skill::Feat(FeatSkill::Tough), 1);

    let mut stats = Stats::empty(body);
    let before = stats.max_health_modifiers.mult_mod;
    skillset.apply_feat_passives(&mut stats);
    // Tough = +0.06 max-health at level 1 -> *= 1.06.
    assert!((stats.max_health_modifiers.mult_mod - before * 1.06).abs() < 1e-5);
}

// ---- BL-06 P2b: Q5 capstone synergy ----

#[cfg(test)]
mod capstone_synergy_tests {
    use super::*;
    use crate::{
        comp::{
            CharacterAbility,
            buff::{BuffData, BuffKind},
            skillset::skills::{RogueSkill, WarriorSkill},
        },
        states::self_buff::BuffDesc,
    };

    /// Build a minimal `SelfBuff` ability with one `BuffDesc` of known
    /// strength.
    fn self_buff_with_strength(strength: f32) -> CharacterAbility {
        CharacterAbility::SelfBuff {
            buildup_duration: 0.1,
            cast_duration: 0.1,
            recover_duration: 0.1,
            buffs: vec![BuffDesc {
                kind: BuffKind::Hastened,
                data: BuffData::new(strength, Some(crate::resources::Secs(5.0))),
            }],
            use_raw_buff_strength: false,
            buff_cat: None,
            energy_cost: 0.0,
            enforced_limit: true,
            combo_cost: 0,
            combo_scaling: None,
            meta: Default::default(),
            specifier: None,
        }
    }

    /// Warrior Onslaught: with BrutalEdge at level 3, strength scales by
    /// 1.0 + 0.08 * 3 = 1.24.
    #[test]
    fn onslaught_synergy_scales_with_brutal_edge() {
        let mut skillset = SkillSet::default();
        skillset
            .skills
            .insert(Skill::Warrior(WarriorSkill::BrutalEdge), 3);

        let ability = self_buff_with_strength(1.0);
        let result = ability.adjusted_by_class_synergy(&skillset, "class.warrior.onslaught");

        if let CharacterAbility::SelfBuff { buffs, .. } = result {
            let strength = buffs[0].data.strength;
            assert!(
                (strength - 1.24).abs() < 1e-5,
                "expected 1.24, got {strength}"
            );
        } else {
            panic!("expected SelfBuff variant");
        }
    }

    /// With no BrutalEdge unlocked (rank 0), scale = 1.0 — no bonus.
    #[test]
    fn onslaught_synergy_zero_rank_no_bonus() {
        let skillset = SkillSet::default();
        let ability = self_buff_with_strength(1.0);
        let result = ability.adjusted_by_class_synergy(&skillset, "class.warrior.onslaught");

        if let CharacterAbility::SelfBuff { buffs, .. } = result {
            let strength = buffs[0].data.strength;
            assert!(
                (strength - 1.0).abs() < 1e-5,
                "expected 1.0 (no bonus), got {strength}"
            );
        } else {
            panic!("expected SelfBuff variant");
        }
    }

    /// Rogue Vanish: with DeadlyPrecision at level 2, strength scales by
    /// 1.0 + 0.08 * 2 = 1.16.
    #[test]
    fn vanish_synergy_scales_with_deadly_precision() {
        let mut skillset = SkillSet::default();
        skillset
            .skills
            .insert(Skill::Rogue(RogueSkill::DeadlyPrecision), 2);

        let ability = self_buff_with_strength(1.0);
        let result = ability.adjusted_by_class_synergy(&skillset, "class.rogue.vanish");

        if let CharacterAbility::SelfBuff { buffs, .. } = result {
            let strength = buffs[0].data.strength;
            assert!(
                (strength - 1.16).abs() < 1e-5,
                "expected 1.16, got {strength}"
            );
        } else {
            panic!("expected SelfBuff variant");
        }
    }

    /// An unknown ability_id leaves the ability unchanged (no synergy applied).
    #[test]
    fn unknown_id_leaves_strength_unchanged() {
        let skillset = SkillSet::default();
        let ability = self_buff_with_strength(2.5);
        let result = ability.adjusted_by_class_synergy(&skillset, "class.warrior.rally");

        if let CharacterAbility::SelfBuff { buffs, .. } = result {
            let strength = buffs[0].data.strength;
            assert!(
                (strength - 2.5).abs() < 1e-5,
                "expected 2.5 (unchanged), got {strength}"
            );
        } else {
            panic!("expected SelfBuff variant");
        }
    }
}
