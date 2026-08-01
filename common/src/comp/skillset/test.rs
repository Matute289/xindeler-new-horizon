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

    // SmitingStrikes folds into both spell_power and the Undead slot of
    // bonus_damage_vs.
    let mut skillset = SkillSet::default();
    skillset
        .skills
        .insert(Skill::Cleric(ClericSkill::SmitingStrikes), 2);
    let mut stats = Stats::empty(human);
    skillset.apply_class_passives(&mut stats);
    assert!((stats.spell_power - 1.08).abs() < 1e-5); // +0.04 spell_power/level
    assert!((stats.bonus_damage_vs[CreatureKind::Undead as usize] - 0.20).abs() < 1e-5); // +0.10/level
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

// ---- Mage tree extension: ManaRecover / ManaFlow / ArcaneVigor / Polyglot,
// and the ManaEfficiency repoint (EnergyReward -> EnergyEfficiency) ----

#[test]
fn mage_tree_extension_new_nodes_apply_expected_stats_deltas() {
    use crate::comp::{Body, Stats, body::humanoid, skills::MageSkill};

    let body = Body::Humanoid(humanoid::Body::iter().next().expect("a humanoid body"));

    // ManaRecover: EnergyReward, 0.06/level, max 3 -> +18% energy on hit.
    // The magnitude is carried forward verbatim from the pre-repoint
    // ManaEfficiency, so this also pins that the shipped balance did not
    // move.
    let mut skillset = SkillSet::default();
    skillset
        .skills
        .insert(Skill::Mage(MageSkill::ManaRecover), 3);
    let mut stats = Stats::empty(body);
    skillset.apply_class_passives(&mut stats);
    assert!((stats.energy_reward_modifier - 1.18).abs() < 1e-5);

    // ManaFlow: EnergyRegen, 0.08/level, max 3 -> +24% regen rate.
    let mut skillset = SkillSet::default();
    skillset.skills.insert(Skill::Mage(MageSkill::ManaFlow), 3);
    let mut stats = Stats::empty(body);
    skillset.apply_class_passives(&mut stats);
    assert!((stats.energy_regen_modifier - 1.24).abs() < 1e-5);

    // ArcaneVigor: MaxHealth, 0.03/level, max 3 -> +9% max health.
    let mut skillset = SkillSet::default();
    skillset
        .skills
        .insert(Skill::Mage(MageSkill::ArcaneVigor), 3);
    let mut stats = Stats::empty(body);
    skillset.apply_class_passives(&mut stats);
    assert!((stats.max_health_modifiers.mult_mod - 1.09).abs() < 1e-5);

    // ManaEfficiency (repointed): EnergyEfficiency, 0.05/level, max 3 ->
    // stats.energy_efficiency_modifier reaches 1.15 (the divisor; see the
    // dedicated cost-reduction test below for what that means in ability
    // terms).
    let mut skillset = SkillSet::default();
    skillset
        .skills
        .insert(Skill::Mage(MageSkill::ManaEfficiency), 3);
    let mut stats = Stats::empty(body);
    skillset.apply_class_passives(&mut stats);
    assert!((stats.energy_efficiency_modifier - 1.15).abs() < 1e-5);

    // Polyglot carries no ClassPassiveStat -- it must not appear in the
    // modifier manifest at all (read via skill_level at the transcription
    // site instead).
    assert!(
        CLASS_SKILL_MODIFIERS
            .get(&Skill::Mage(MageSkill::Polyglot))
            .is_none(),
        "Polyglot must have no class_skill_modifiers.ron entry",
    );
}

#[test]
fn mana_efficiency_divisor_yields_13_percent_not_15_percent() {
    // EnergyEfficiency is a DIVISOR (`*energy_cost /= stats.energy_efficiency`),
    // so a naive reading of "0.05/level x 3 = +15%" is wrong: max-rank
    // ManaEfficiency must cut a real ability's energy_cost by 13.0%, not 15%.
    use crate::{
        comp::{
            Body, CharacterAbility, Stats,
            body::humanoid,
            buff::{BuffData, BuffKind},
            inventory::item::tool,
            skills::MageSkill,
        },
        resources::Secs,
        states::self_buff::BuffDesc,
    };

    let body = Body::Humanoid(humanoid::Body::iter().next().expect("a humanoid body"));
    let mut skillset = SkillSet::default();
    skillset
        .skills
        .insert(Skill::Mage(MageSkill::ManaEfficiency), 3);
    let mut player_stats = Stats::empty(body);
    skillset.apply_class_passives(&mut player_stats);
    assert!((player_stats.energy_efficiency_modifier - 1.15).abs() < 1e-5);

    let ability = CharacterAbility::SelfBuff {
        buildup_duration: 0.1,
        cast_duration: 0.1,
        recover_duration: 0.1,
        buffs: vec![BuffDesc {
            kind: BuffKind::Hastened,
            data: BuffData::new(1.0, Some(Secs(5.0))),
        }],
        use_raw_buff_strength: false,
        buff_cat: None,
        energy_cost: 100.0,
        enforced_limit: true,
        combo_cost: 0,
        combo_scaling: None,
        meta: Default::default(),
        specifier: None,
    };

    let mut contextual_stats = tool::Stats::one();
    contextual_stats.energy_efficiency *= player_stats.energy_efficiency_modifier;
    let adjusted = ability.adjusted_by_stats(contextual_stats);

    let CharacterAbility::SelfBuff { energy_cost, .. } = adjusted else {
        panic!("expected SelfBuff variant");
    };
    // 100 / 1.15 = 86.9565... -- a 13.0% reduction, NOT 15%.
    assert!(
        (energy_cost - 86.9565).abs() < 0.01,
        "expected energy_cost ~86.96, got {energy_cost}"
    );
    let pct_reduction = (100.0 - energy_cost) / 100.0 * 100.0;
    assert!(
        (pct_reduction - 13.0).abs() < 0.1,
        "expected a ~13.0% cost reduction, got {pct_reduction}% (15% would be the wrong, naive \
         reading)"
    );
}

#[test]
fn every_mage_skill_resolves_a_max_level() {
    use crate::comp::{class::ClassKind, skills::MageSkill};

    // Asset-walk: every skill listed in the Class(Mage) group must resolve a
    // real max_level. Active/capstone skills (ArcaneSurge, Overcharge,
    // ArcaneMastery) are deliberately absent from skill_max_levels.ron and
    // fall back to the default of 1 (Skill::max_level's documented
    // behaviour); every other (passive) skill must have an EXPLICIT entry so
    // a missing row doesn't silently default to 1.
    let known_actives = [
        Skill::Mage(MageSkill::ArcaneSurge),
        Skill::Mage(MageSkill::Overcharge),
        Skill::Mage(MageSkill::ArcaneMastery),
    ];

    let mage_skills = &SKILL_GROUP_DEFS
        .get(&SkillGroupKind::Class(ClassKind::Mage))
        .expect("Class(Mage) must be defined in the skill-groups manifest")
        .skills;
    for skill in mage_skills {
        if known_actives.contains(skill) {
            continue;
        }
        assert!(
            SKILL_MAX_LEVEL.contains_key(skill),
            "{skill:?} is a passive Mage skill and must have an explicit skill_max_levels.ron \
             entry (found none -- would silently default to 1)",
        );
    }

    // The four new nodes specifically, at their intended max level.
    for skill in [
        Skill::Mage(MageSkill::ManaRecover),
        Skill::Mage(MageSkill::ManaFlow),
        Skill::Mage(MageSkill::ArcaneVigor),
        Skill::Mage(MageSkill::Polyglot),
    ] {
        assert_eq!(
            SKILL_MAX_LEVEL.get(&skill).copied(),
            Some(3),
            "{skill:?} must have max_level: 3",
        );
    }
}

#[test]
fn arcane_vigor_max_keeps_mage_the_lowest_hp_class_at_level_60() {
    use crate::comp::{
        Body, Stats,
        body::humanoid,
        class::{self, ClassKind},
        skills::ClassPassiveStat,
    };

    let body = Body::Humanoid(humanoid::Body::random());
    let manifest = class::class_attributes_manifest();

    let hp_at_60 = |class_kind: ClassKind| -> f32 {
        let mut stats = Stats::empty(body);
        let attrs =
            manifest.0.get(&class_kind).copied().unwrap_or_else(|| {
                panic!("{class_kind:?} must be defined in class_attributes.ron")
            });
        attrs.apply(&mut stats, 60, true);
        if class_kind == ClassKind::Mage {
            // ArcaneVigor at max rank 3: 0.03/level -> +9%.
            ClassPassiveStat::MaxHealth.apply(&mut stats, 0.03 * 3.0);
        }
        stats
            .max_health_modifiers
            .compute_maximum(body.base_health() as f32)
    };

    let mage_hp = hp_at_60(ClassKind::Mage);
    let sorcerer_hp = hp_at_60(ClassKind::Sorcerer);

    let mut third_lowest = f32::MAX;
    for &class_kind in manifest.0.keys() {
        if class_kind == ClassKind::Mage || class_kind == ClassKind::Sorcerer {
            continue;
        }
        let hp = hp_at_60(class_kind);
        assert!(
            mage_hp < hp,
            "Mage HP ({mage_hp}) at L60 must stay below {class_kind:?} ({hp}) even with \
             ArcaneVigor at max rank",
        );
        third_lowest = third_lowest.min(hp);
    }

    // "Tied with Sorcerer": the Mage/Sorcerer gap must be far smaller than
    // the gap from either of them to the next class up.
    let mage_sorcerer_gap = (mage_hp - sorcerer_hp).abs();
    let gap_to_third = third_lowest - mage_hp.max(sorcerer_hp);
    assert!(
        mage_sorcerer_gap < gap_to_third,
        "Mage/Sorcerer HP gap ({mage_sorcerer_gap}) should be far smaller than the gap to the \
         third-lowest class ({gap_to_third}), i.e. they should remain tied for last",
    );
}

#[test]
fn adding_the_four_new_mage_nodes_changed_the_class_group_hash() {
    // SKILL_GROUP_HASHES SHA-256s a group's `(skill, max_level)` membership
    // list; a changed hash force-respecs (and refunds) every character
    // holding that group on load. Adding ManaRecover/ManaFlow/ArcaneVigor/
    // Polyglot to Class(Mage) MUST move the hash -- a silent non-change would
    // mean the repointed ManaEfficiency node quietly changed meaning under
    // already-invested points with no refund, which is the one bad outcome
    // this mechanism exists to prevent.
    use crate::comp::{class::ClassKind, skills::MageSkill};
    use sha2::{Digest, Sha256};
    use std::collections::BTreeSet;

    // The pre-extension Class(Mage) membership (12 nodes). ManaEfficiency's
    // *meaning* was repointed by this same change (EnergyReward ->
    // EnergyEfficiency) but its variant name and max_level (3) are
    // unchanged, so it round-trips into this reconstruction unchanged --
    // only the group's skill MEMBERSHIP is what moves the hash.
    let pre_extension: BTreeSet<Skill> = [
        Skill::Mage(MageSkill::FocusedMind),
        Skill::Mage(MageSkill::TrueAim),
        Skill::Mage(MageSkill::ArcaneSurge),
        Skill::Mage(MageSkill::SpellPotency),
        Skill::Mage(MageSkill::PyromanticAttunement),
        Skill::Mage(MageSkill::CryomanticAttunement),
        Skill::Mage(MageSkill::QuickCasting),
        Skill::Mage(MageSkill::PenetratingMagic),
        Skill::Mage(MageSkill::WardedSkin),
        Skill::Mage(MageSkill::ManaEfficiency),
        Skill::Mage(MageSkill::Overcharge),
        Skill::Mage(MageSkill::ArcaneMastery),
    ]
    .into_iter()
    .collect();

    let pre_extension_json: Vec<_> = pre_extension
        .iter()
        .map(|skill| (*skill, skill.max_level()))
        .collect();
    let mut hasher = Sha256::new();
    hasher.update(
        serde_json::to_string(&pre_extension_json)
            .unwrap()
            .as_bytes(),
    );
    let pre_extension_hash: Vec<u8> = hasher.finalize().iter().copied().collect();

    let live_hash = SKILL_GROUP_HASHES
        .get(&SkillGroupKind::Class(ClassKind::Mage))
        .expect("Class(Mage) must have a hash entry")
        .clone();

    assert_ne!(
        pre_extension_hash, live_hash,
        "adding ManaRecover/ManaFlow/ArcaneVigor/Polyglot must change the Class(Mage) skill-group \
         hash so every persisted Mage is force-respec'd and refunded",
    );

    let live_len = SKILL_GROUP_DEFS
        .get(&SkillGroupKind::Class(ClassKind::Mage))
        .expect("Class(Mage) must be defined")
        .skills
        .len();
    assert_eq!(
        live_len, 16,
        "Class(Mage) should now have 16 nodes (12 existing + 4 new)"
    );
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
