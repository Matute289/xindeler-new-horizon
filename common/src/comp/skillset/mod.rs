use crate::{
    assets::{AssetExt, Ron},
    comp::{
        Stats,
        class::ClassKind,
        item::tool::ToolKind,
        skills::{ClassPassiveStat, Skill},
    },
};
use core::borrow::{Borrow, BorrowMut};
use hashbrown::HashMap;
use lazy_static::lazy_static;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use specs::{Component, DerefFlaggedStorage};
use std::{collections::BTreeSet, hash::Hash};
use tracing::{trace, warn};

pub mod skills;

#[cfg(test)] mod test;

/// Maximum character level (WoW/Diablo-style derived level — see
/// docs/design/specs/2026-06-10-character-levels-design.md).
pub const MAX_CHARACTER_LEVEL: u16 = 60;
/// Cumulative XP required for level L is LEVEL_XP_BASE * (L - 1)^2.
pub const LEVEL_XP_BASE: u32 = 250;

/// Character level for a given lifetime (earned) XP total. Level 1 at 0 XP,
/// capped at MAX_CHARACTER_LEVEL. Inverse of [`total_exp_for_level`].
pub fn level_from_total_exp(total_exp: u32) -> u16 {
    let raw = ((total_exp / LEVEL_XP_BASE) as f64).sqrt() as u16 + 1;
    raw.min(MAX_CHARACTER_LEVEL)
}

/// Cumulative lifetime XP at which the given level is reached.
pub fn total_exp_for_level(level: u16) -> u32 {
    let l = u32::from(level.clamp(1, MAX_CHARACTER_LEVEL)) - 1;
    LEVEL_XP_BASE.saturating_mul(l).saturating_mul(l)
}

pub struct SkillGroupDef {
    pub skills: BTreeSet<Skill>,
    pub total_skill_point_cost: u16,
}

lazy_static! {
    // Determines the skills that comprise each skill group.
    //
    // This data is used to determine which of a player's skill groups a
    // particular skill should be added to when a skill unlock is requested.
    pub static ref SKILL_GROUP_DEFS: HashMap<SkillGroupKind, SkillGroupDef> = {
        // BTreeSet is used here to ensure that skills are ordered. This is important
        // to ensure that the hash created from it is consistent so that we don't
        // needlessly force a respec when loading skills from persistence.
        let map: HashMap<SkillGroupKind, BTreeSet<Skill>> = Ron::load_expect_cloned(
            "common.skill_trees.skills_skill-groups_manifest",
        ).into_inner();
        map.iter().map(|(sgk, skills)|
            (*sgk, SkillGroupDef { skills: skills.clone(),
                total_skill_point_cost: skills
                    .iter()
                    .map(|skill| {
                        let max_level = skill.max_level();
                        (1..=max_level)
                            .map(|level| skill.skill_cost(level))
                            .sum::<u16>()
                    })
                    .sum()
            })
        )
        .collect()
    };
    // Creates a hashmap for the reverse lookup of skill groups from a skill
    pub static ref SKILL_GROUP_LOOKUP: HashMap<Skill, SkillGroupKind> = {
        let map: HashMap<SkillGroupKind, BTreeSet<Skill>> = Ron::load_expect_cloned(
            "common.skill_trees.skills_skill-groups_manifest",
        ).into_inner();
        map.iter().flat_map(|(sgk, skills)| skills.iter().map(move |s| (*s, *sgk))).collect()
    };
    // Loads the maximum level that a skill can obtain
    pub static ref SKILL_MAX_LEVEL: HashMap<Skill, u16> = {
        Ron::load_expect_cloned(
            "common.skill_trees.skill_max_levels",
        ).into_inner()
    };
    /// Loads the prerequisite skills for each skill. It cannot currently detect
    /// cyclic dependencies, so if you modify the prerequisite map ensure that there
    /// are no cycles of prerequisites.
    pub static ref SKILL_PREREQUISITES: HashMap<Skill, SkillPrerequisite> = {
        Ron::load_expect_cloned(
            "common.skill_trees.skill_prerequisites",
        ).into_inner()
    };
    pub static ref SKILL_GROUP_HASHES: HashMap<SkillGroupKind, Vec<u8>> = {
        let map: HashMap<SkillGroupKind, BTreeSet<Skill>> = Ron::load_expect_cloned(
            "common.skill_trees.skills_skill-groups_manifest",
        ).into_inner();
        let mut hashes = HashMap::new();
        for (skill_group_kind, skills) in map.iter() {
            let mut hasher = Sha256::new();
            let json_input: Vec<_> = skills.iter().map(|skill| (*skill, skill.max_level())).collect();
            let hash_input = serde_json::to_string(&json_input).unwrap_or_default();
            hasher.update(hash_input.as_bytes());
            let hash_result = hasher.finalize();
            hashes.insert(*skill_group_kind, hash_result.iter().copied().collect());
        }
        hashes
    };
    /// BL-06: per-level `Stats` modifiers for passive class skills. Maps a class
    /// skill to the field(s) it boosts and the magnitude added per skill level.
    /// Active-ability skills are absent (they unlock abilities, not stats).
    pub static ref CLASS_SKILL_MODIFIERS: HashMap<Skill, Vec<(ClassPassiveStat, f32)>> = {
        Ron::load_expect_cloned(
            "common.skill_trees.class_skill_modifiers",
        ).into_inner()
    };
    /// BL-20: per-level `Stats` modifiers for passive feats. Maps a feat skill
    /// to the field(s) it boosts and the magnitude added per skill level (all
    /// feats are max_level = 1, so this degenerates to one magnitude per
    /// purchased feat). Active-ability feats are absent (they unlock
    /// abilities, not stats).
    pub static ref FEAT_MODIFIERS: HashMap<Skill, Vec<(ClassPassiveStat, f32)>> = {
        Ron::load_expect_cloned(
            "common.skill_trees.feat_modifiers",
        ).into_inner()
    };
}

#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq, Serialize, Deserialize, Ord, PartialOrd)]
pub enum SkillGroupKind {
    General,
    Weapon(ToolKind),
    Class(ClassKind),
    // BL-20: a single, class-agnostic group available to every character
    // (parallel to `General`). Points are granted via level-milestone
    // (`grant_skill_point`), never via the XP-per-group economy.
    Feats,
}

impl SkillGroupKind {
    /// Gets the cost in experience of earning a skill point
    /// Changing this is forward compatible with persistence and will
    /// automatically force a respec for skill group kinds that are affected.
    pub fn skill_point_cost(self, level: u16) -> u32 {
        use std::f32::consts::E;
        match self {
            Self::Weapon(ToolKind::Sword | ToolKind::Axe | ToolKind::Hammer | ToolKind::Bow)
            | Self::Class(_) => {
                let level = level as f32;
                ((400.0 * (level / (level + 20.0)).powi(2) + 5.0 * E.powf(0.025 * level))
                    .min(u32::MAX as f32) as u32)
                    .saturating_mul(25)
            },
            _ => {
                const EXP_INCREMENT: f32 = 10.0;
                const STARTING_EXP: f32 = 70.0;
                const EXP_CEILING: f32 = 1000.0;
                const SCALING_FACTOR: f32 = 0.125;
                (EXP_INCREMENT
                    * (EXP_CEILING
                        / EXP_INCREMENT
                        / (1.0
                            + E.powf(-SCALING_FACTOR * level as f32)
                                * (EXP_CEILING / STARTING_EXP - 1.0)))
                        .floor()) as u32
            },
        }
    }

    /// Gets the total amount of skill points that can be spent in a particular
    /// skill group
    pub fn total_skill_point_cost(self) -> u16 {
        if let Some(SkillGroupDef {
            total_skill_point_cost,
            ..
        }) = SKILL_GROUP_DEFS.get(&self)
        {
            *total_skill_point_cost
        } else {
            0
        }
    }
}

/// A group of skills that have been unlocked by a player. Each skill group has
/// independent exp and skill points which are used to unlock skills in that
/// skill group.
#[derive(Clone, Debug, Eq, Hash, PartialEq, Serialize, Deserialize)]
pub struct SkillGroup {
    // Invariant should be maintained that this is the same kind as the key that the skill group is
    // inserted into the skillset as.
    pub skill_group_kind: SkillGroupKind,
    // The invariant that (earned_exp >= available_exp) should not be violated
    pub available_exp: u32,
    pub earned_exp: u32,
    // The invariant that (earned_sp >= available_sp) should not be violated
    pub available_sp: u16,
    pub earned_sp: u16,
    // Used for persistence
    pub ordered_skills: Vec<Skill>,
}

impl SkillGroup {
    fn new(skill_group_kind: SkillGroupKind) -> SkillGroup {
        SkillGroup {
            skill_group_kind,
            available_exp: 0,
            earned_exp: 0,
            available_sp: 0,
            earned_sp: 0,
            ordered_skills: Vec::new(),
        }
    }

    /// Returns the amount of experience in a skill group that has been spent to
    /// acquire skill points Relies on the invariant that (earned_exp >=
    /// available_exp) to ensure function does not underflow
    pub fn spent_exp(&self) -> u32 { self.earned_exp - self.available_exp }

    /// Adds a skill point while subtracting the necessary amount of experience
    fn earn_skill_point(&mut self) -> Result<(), SpRewardError> {
        let sp_cost = self.skill_group_kind.skill_point_cost(self.earned_sp);
        // If there is insufficient available exp, checked sub will fail as the result
        // would be less than 0
        let new_available_exp = self
            .available_exp
            .checked_sub(sp_cost)
            .ok_or(SpRewardError::InsufficientExp)?;
        let new_earned_sp = self
            .earned_sp
            .checked_add(1)
            .ok_or(SpRewardError::Overflow)?;
        let new_available_sp = self
            .available_sp
            .checked_add(1)
            .ok_or(SpRewardError::Overflow)?;
        self.available_exp = new_available_exp;
        self.earned_sp = new_earned_sp;
        self.available_sp = new_available_sp;
        Ok(())
    }

    /// Also attempts to earn a skill point after adding experience. If a skill
    /// point was earned, returns how many skill points the skill group now has
    /// earned in total.
    pub fn add_experience(&mut self, amount: u32) -> Option<u16> {
        self.earned_exp = self.earned_exp.saturating_add(amount);
        self.available_exp = self.available_exp.saturating_add(amount);

        let mut return_val = None;
        // Attempt to earn skill point
        while self.earn_skill_point().is_ok() {
            return_val = Some(self.earned_sp);
        }
        return_val
    }
}

/// Contains all of a player's skill groups and skills. Provides methods for
/// manipulating assigned skills and skill groups including unlocking skills,
/// refunding skills etc.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(from = "SkillSetMeta")]
pub struct SkillSet {
    skill_groups: HashMap<SkillGroupKind, SkillGroup>,
    skills: HashMap<Skill, u16>,
    /// Cached derived character level (see [`SkillSet::character_level`]).
    /// Recomputed whenever a skill group's lifetime `earned_exp` changes, and
    /// rebuilt from XP on deserialize. NEVER persisted or sent over the wire
    /// (`#[serde(skip)]`) — the level is always derived, so it can't desync.
    #[serde(skip)]
    character_level: u16,
}

/// Deserialization shadow for [`SkillSet`]: carries only the persisted/synced
/// fields. The derived `character_level` cache is rebuilt from XP via `From`,
/// so it never has to be trusted from the wire or disk.
#[derive(Deserialize)]
struct SkillSetMeta {
    skill_groups: HashMap<SkillGroupKind, SkillGroup>,
    skills: HashMap<Skill, u16>,
}

impl From<SkillSetMeta> for SkillSet {
    fn from(meta: SkillSetMeta) -> Self {
        let mut skill_set = SkillSet {
            skill_groups: meta.skill_groups,
            skills: meta.skills,
            character_level: 0,
        };
        skill_set.recompute_character_level();
        skill_set
    }
}

impl Component for SkillSet {
    type Storage = DerefFlaggedStorage<Self, specs::VecStorage<Self>>;
}

impl Default for SkillSet {
    /// Instantiate a new skill set with the default skill groups with no
    /// unlocked skills in them - used when adding a skill set to a new
    /// player
    fn default() -> Self {
        // Create an empty skillset
        let mut skill_group = Self {
            skill_groups: HashMap::new(),
            skills: SkillSet::initial_skills(),
            character_level: 0,
        };

        // Insert default skill groups
        skill_group.unlock_skill_group(SkillGroupKind::General);
        skill_group.unlock_skill_group(SkillGroupKind::Weapon(ToolKind::Pick));
        // BL-20: every character should see the Feats group from creation, even
        // with zero points (points arrive later via level-milestone grants).
        skill_group.unlock_skill_group(SkillGroupKind::Feats);

        // No XP yet — derive the level-1 baseline (unlocking groups doesn't
        // touch earned_exp, so a single recompute here suffices).
        skill_group.recompute_character_level();
        skill_group
    }
}

impl SkillSet {
    pub fn initial_skills() -> HashMap<Skill, u16> {
        let mut skills = HashMap::new();
        skills.insert(Skill::UnlockGroup(SkillGroupKind::General), 1);
        skills.insert(
            Skill::UnlockGroup(SkillGroupKind::Weapon(ToolKind::Pick)),
            1,
        );
        skills
    }

    /// NOTE: This does *not* return an error on failure, since we can partially
    /// recover from some failures.  Instead, it returns the error in the
    /// second return value; make sure to handle it if present!
    pub fn load_from_database(
        skill_groups: HashMap<SkillGroupKind, SkillGroup>,
        mut all_skills: HashMap<SkillGroupKind, Result<Vec<Skill>, SkillsPersistenceError>>,
    ) -> (Self, Option<SkillsPersistenceError>) {
        let mut skillset = SkillSet {
            skill_groups,
            skills: SkillSet::initial_skills(),
            character_level: 0,
        };
        // Lifetime XP is fixed by the restored skill groups and is never mutated
        // below (skill unlocking only spends SP), so derive the cache once here.
        skillset.recompute_character_level();
        let mut persistence_load_error = None;

        // Class skill groups are granted directly (unlock_skill_group at creation /
        // /set_class), not earned through the skill tree, so seed their unlock skill
        // here — both the loop below and class-gated abilities key off
        // has_skill(UnlockGroup(..)). Also repairs characters saved before this
        // seeding.
        let class_groups: Vec<SkillGroupKind> = skillset
            .skill_groups
            .keys()
            .copied()
            .filter(|kind| matches!(kind, SkillGroupKind::Class(_)))
            .collect();
        for kind in class_groups {
            skillset.skills.entry(Skill::UnlockGroup(kind)).or_insert(1);
        }

        // Loops while checking the all_skills hashmap. For as long as it can find an
        // entry where the skill group kind is unlocked, insert the skills corresponding
        // to that skill group kind. When no more skill group kinds can be found, break
        // the loop.
        while let Some(skill_group_kind) = all_skills
            .keys()
            .find(|kind| skillset.has_skill(Skill::UnlockGroup(**kind)))
            .copied()
        {
            // Remove valid skill group kind from the hash map so that loop eventually
            // terminates.
            if let Some(skills_result) = all_skills.remove(&skill_group_kind) {
                match skills_result {
                    Ok(skills) => {
                        let backup_skillset = skillset.clone();
                        // Iterate over all skills and make sure that unlocking them is successful.
                        // If any fail, fall back to skillset before
                        // unlocking any to allow a full respec
                        if !skills
                            .iter()
                            .all(|skill| skillset.unlock_skill(*skill).is_ok())
                        {
                            skillset = backup_skillset;
                            // If unlocking failed, set persistence_load_error
                            persistence_load_error =
                                Some(SkillsPersistenceError::SkillsUnlockFailed)
                        }
                    },
                    Err(persistence_error) => persistence_load_error = Some(persistence_error),
                }
            }
        }

        (skillset, persistence_load_error)
    }

    /// Check if a particular skill group is accessible for an entity, *if* it
    /// exists.
    fn skill_group_accessible_if_exists(&self, skill_group_kind: SkillGroupKind) -> bool {
        self.has_skill(Skill::UnlockGroup(skill_group_kind))
    }

    /// Checks if a particular skill group is accessible for an entity
    pub fn skill_group_accessible(&self, skill_group_kind: SkillGroupKind) -> bool {
        self.skill_groups.contains_key(&skill_group_kind)
            && self.skill_group_accessible_if_exists(skill_group_kind)
    }

    /// Unlocks a skill group directly. Used by character creation and
    /// /set_class; players unlock weapon groups via Skill::UnlockGroup instead.
    pub fn unlock_skill_group(&mut self, skill_group_kind: SkillGroupKind) {
        if !self.skill_groups.contains_key(&skill_group_kind) {
            self.skill_groups
                .insert(skill_group_kind, SkillGroup::new(skill_group_kind));
        }
        // Record the group's unlock skill so `has_skill(UnlockGroup(..))` reflects
        // membership. Class-gated abilities key off this, and load_from_database's
        // skill-loading loop already assumes it. Symmetric with the General group.
        self.skills
            .entry(Skill::UnlockGroup(skill_group_kind))
            .or_insert(1);
    }

    /// Returns an iterator over skill groups
    pub fn skill_groups(&self) -> impl Iterator<Item = &SkillGroup> { self.skill_groups.values() }

    /// Returns a reference to a particular skill group in a skillset
    fn skill_group(&self, skill_group: SkillGroupKind) -> Option<&SkillGroup> {
        self.skill_groups.get(&skill_group)
    }

    /// Returns a mutable reference to a particular skill group in a skillset
    /// Requires that skillset contains skill that unlocks the skill group
    fn skill_group_mut(&mut self, skill_group: SkillGroupKind) -> Option<&mut SkillGroup> {
        // In order to mutate skill group, we check that the prerequisite skill has been
        // acquired, as this is one of the requirements for us to consider the skill
        // group accessible.
        let skill_group_accessible = self.skill_group_accessible(skill_group);
        if skill_group_accessible {
            self.skill_groups.get_mut(&skill_group)
        } else {
            None
        }
    }

    /// Adds experience to the skill group within an entity's skill set, will
    /// attempt to earn a skill point while doing so. If a skill point was
    /// earned, returns the number of earned skill points in the skill group.
    pub fn add_experience(&mut self, skill_group_kind: SkillGroupKind, amount: u32) -> Option<u16> {
        if let Some(skill_group) = self.skill_group_mut(skill_group_kind) {
            let earned_sp = skill_group.add_experience(amount);
            // Lifetime XP just grew → refresh the derived-level cache.
            self.recompute_character_level();
            earned_sp
        } else {
            warn!("Tried to add experience to a skill group that player does not have");
            None
        }
    }

    /// Lifetime XP earned across all skill groups. Drives the derived
    /// character level; relies on `earned_exp` being monotonically
    /// non-decreasing and persisted per skill group.
    pub fn total_earned_exp(&self) -> u32 {
        self.skill_groups
            .values()
            .map(|sg| sg.earned_exp)
            .fold(0, u32::saturating_add)
    }

    /// Derived character level (1..=MAX_CHARACTER_LEVEL). Not persisted —
    /// always derived from lifetime XP so it can never desync. Returns the
    /// cached value (maintained by [`SkillSet::recompute_character_level`] on
    /// every `earned_exp` mutation), so callers on the tick path pay no fold.
    pub fn character_level(&self) -> u16 { self.character_level }

    /// Recompute the cached derived character level from lifetime XP. Must be
    /// called after any change to a skill group's `earned_exp`; this is the
    /// only place [`SkillSet::character_level`] is folded.
    fn recompute_character_level(&mut self) {
        self.character_level = level_from_total_exp(self.total_earned_exp());
    }

    /// Set the character's derived level by adjusting the **General** skill
    /// group's earned XP so `total_earned_exp()` equals
    /// `total_exp_for_level(level)` (clamped to `1..=MAX_CHARACTER_LEVEL`).
    /// Intended for admin/test setup (the `/set_level` command). Other
    /// skill groups' earned XP is preserved — if it already exceeds the
    /// target the resulting level may be higher. Spent exp on the General
    /// group is preserved; the remainder becomes available to spend.
    pub fn set_level(&mut self, level: u16) {
        let level = level.clamp(1, MAX_CHARACTER_LEVEL);
        let target = total_exp_for_level(level);
        let general = SkillGroupKind::General;
        let general_earned = self
            .skill_groups
            .get(&general)
            .map_or(0, |sg| sg.earned_exp);
        let other_earned = self.total_earned_exp().saturating_sub(general_earned);
        let new_general_earned = target.saturating_sub(other_earned);
        if let Some(sg) = self.skill_groups.get_mut(&general) {
            // Never drop earned_exp below what's already been spent on skill points:
            // doing so would make `spent_exp()` silently shrink while the unlocked
            // skills + earned_sp remain, desyncing the live component from its
            // persisted form (a forced respec on relog). Floor at `spent` instead —
            // a down-level request below that floor just can't go lower (rare; the
            // General tree has no purchasable skills in v1, so spent is normally 0).
            let spent = sg.spent_exp();
            sg.earned_exp = new_general_earned.max(spent);
            sg.available_exp = sg.earned_exp - spent;
        }
        // General-group earned_exp changed → refresh the derived-level cache.
        self.recompute_character_level();
    }

    /// Gets the available experience for a particular skill group
    pub fn available_experience(&self, skill_group: SkillGroupKind) -> u32 {
        self.skill_group(skill_group)
            .map_or(0, |s_g| s_g.available_exp)
    }

    /// Checks how much experience is needed for the next skill point in a tree
    pub fn skill_point_cost(&self, skill_group: SkillGroupKind) -> u32 {
        if let Some(level) = self.skill_group(skill_group).map(|sg| sg.earned_sp) {
            skill_group.skill_point_cost(level)
        } else {
            skill_group.skill_point_cost(0)
        }
    }

    /// Adds skill points to a skill group as long as the player has that skill
    /// group type.
    pub fn add_skill_points(
        &mut self,
        skill_group_kind: SkillGroupKind,
        number_of_skill_points: u16,
    ) {
        for _ in 0..number_of_skill_points {
            let exp_needed = self.skill_point_cost(skill_group_kind);
            self.add_experience(skill_group_kind, exp_needed);
        }
    }

    /// BL-20: grants one skill point to `kind` directly, bypassing the
    /// XP-cost economy entirely. Used for milestone-based grants (e.g. "1
    /// feat point per 10 character levels"), as opposed to
    /// [`Self::add_skill_points`]/[`SkillGroup::earn_skill_point`] which are
    /// exp-driven. Unlocks the group first if it doesn't exist yet.
    pub fn grant_skill_point(&mut self, kind: SkillGroupKind) {
        self.unlock_skill_group(kind);
        if let Some(skill_group) = self.skill_groups.get_mut(&kind) {
            skill_group.earned_sp = skill_group.earned_sp.saturating_add(1);
            skill_group.available_sp = skill_group.available_sp.saturating_add(1);
        }
    }

    /// Gets the available points for a particular skill group
    pub fn available_sp(&self, skill_group: SkillGroupKind) -> u16 {
        self.skill_group(skill_group)
            .map_or(0, |s_g| s_g.available_sp)
    }

    /// Gets the total earned points for a particular skill group
    pub fn earned_sp(&self, skill_group: SkillGroupKind) -> u16 {
        self.skill_group(skill_group).map_or(0, |s_g| s_g.earned_sp)
    }

    /// Checks that the skill set contains all prerequisite skills of the
    /// required level for a particular skill
    pub fn prerequisites_met(&self, skill: Skill) -> bool {
        match skill.prerequisite_skills() {
            Some(SkillPrerequisite::All(skills)) => skills
                .iter()
                .all(|(s, l)| self.skill_level(*s).is_ok_and(|l_b| l_b >= *l)),
            Some(SkillPrerequisite::Any(skills)) => skills
                .iter()
                .any(|(s, l)| self.skill_level(*s).is_ok_and(|l_b| l_b >= *l)),
            None => true,
        }
    }

    /// Gets skill point cost to purchase skill of next level
    pub fn skill_cost(&self, skill: Skill) -> u16 {
        let next_level = self.next_skill_level(skill);
        skill.skill_cost(next_level)
    }

    /// Checks if player has sufficient skill points to purchase a skill
    pub fn sufficient_skill_points(&self, skill: Skill) -> bool {
        if let Some(skill_group_kind) = skill.skill_group_kind() {
            if let Some(skill_group) = self.skill_group(skill_group_kind) {
                let needed_sp = self.skill_cost(skill);
                skill_group.available_sp >= needed_sp
            } else {
                false
            }
        } else {
            false
        }
    }

    /// Checks the next level of a skill
    fn next_skill_level(&self, skill: Skill) -> u16 {
        if let Ok(level) = self.skill_level(skill) {
            // If already has skill, then level + 1
            level + 1
        } else {
            // Otherwise the next level is the first level
            1
        }
    }

    /// Unlocks a skill for a player, assuming they have the relevant skill
    /// group unlocked and available SP in that skill group.
    ///
    /// NOTE: Please don't use pathological or clever implementations of to_mut
    /// here.
    pub fn unlock_skill_cow<'a, B, C>(
        this_: &'a mut B,
        skill: Skill,
        to_mut: impl FnOnce(&'a mut B) -> &'a mut C,
    ) -> Result<(), SkillUnlockError>
    where
        B: Borrow<SkillSet>,
        C: BorrowMut<SkillSet> + 'a,
    {
        if let Some(skill_group_kind) = skill.skill_group_kind() {
            let this = (*this_).borrow();
            let next_level = this.next_skill_level(skill);
            let prerequisites_met = this.prerequisites_met(skill);
            // Check that skill is not yet at max level
            if !matches!(this.skills.get(&skill), Some(level) if *level == skill.max_level()) {
                if let Some(skill_group) = this.skill_groups.get(&skill_group_kind)
                    && this.skill_group_accessible_if_exists(skill_group_kind)
                {
                    if prerequisites_met {
                        if let Some(new_available_sp) = skill_group
                            .available_sp
                            .checked_sub(skill.skill_cost(next_level))
                        {
                            // Perform all mutation inside this branch, to avoid triggering a copy
                            // on write or flagged storage in cases where this matters.
                            let this_ = to_mut(this_);
                            let this = this_.borrow_mut();
                            // NOTE: Verified to exist previously when we accessed
                            // this.skill_groups (assuming a non-pathological implementation of
                            // ToOwned).
                            let skill_group = this.skill_groups.get_mut(&skill_group_kind).expect(
                                "Verified to exist when we previously accessed this.skill_groups",
                            );
                            skill_group.available_sp = new_available_sp;
                            skill_group.ordered_skills.push(skill);
                            if let Skill::UnlockGroup(group) = skill {
                                this.unlock_skill_group(group);
                            }
                            this.skills.insert(skill, next_level);
                            Ok(())
                        } else {
                            trace!("Tried to unlock skill for skill group with insufficient SP");
                            Err(SkillUnlockError::InsufficientSP)
                        }
                    } else {
                        trace!("Tried to unlock skill without meeting prerequisite skills");
                        Err(SkillUnlockError::MissingPrerequisites)
                    }
                } else {
                    trace!("Tried to unlock skill for a skill group that player does not have");
                    Err(SkillUnlockError::UnavailableSkillGroup)
                }
            } else {
                trace!("Tried to unlock skill the player already has");
                Err(SkillUnlockError::SkillAlreadyUnlocked)
            }
        } else {
            warn!(
                ?skill,
                "Tried to unlock skill that does not exist in any skill group!"
            );
            Err(SkillUnlockError::NoParentSkillTree)
        }
    }

    /// Convenience function for the case where you have mutable access to the
    /// skill.
    pub fn unlock_skill(&mut self, skill: Skill) -> Result<(), SkillUnlockError> {
        Self::unlock_skill_cow(self, skill, |x| x)
    }

    /// Checks if the player has available SP to spend
    pub fn has_available_sp(&self) -> bool {
        self.skill_groups.iter().any(|(kind, sg)| {
            sg.available_sp > 0
            // Subtraction in bounds because of the invariant that available_sp <= earned_sp
                && (sg.earned_sp - sg.available_sp) < kind.total_skill_point_cost()
        })
    }

    /// Checks if the player can actually spend SP to unlock a skill
    pub fn can_unlock_any_skill(&self) -> bool {
        SKILL_GROUP_DEFS.iter().any(|(skill_group_kind, def)| {
            let available_sp = self.available_sp(*skill_group_kind);

            if available_sp == 0 {
                return false;
            }

            def.skills.iter().any(|skill| {
                !self.is_at_max_level(*skill)
                    && self.prerequisites_met(*skill)
                    && self.skill_cost(*skill) <= available_sp
            })
        })
    }

    /// Checks if the skill is at max level in a skill set
    pub fn is_at_max_level(&self, skill: Skill) -> bool {
        if let Ok(level) = self.skill_level(skill) {
            level == skill.max_level()
        } else {
            false
        }
    }

    /// Checks if skill set contains a skill
    pub fn has_skill(&self, skill: Skill) -> bool { self.skills.contains_key(&skill) }

    /// Returns the level of the skill
    pub fn skill_level(&self, skill: Skill) -> Result<u16, SkillError> {
        if let Some(level) = self.skills.get(&skill).copied() {
            Ok(level)
        } else {
            Err(SkillError::MissingSkill)
        }
    }

    /// BL-06: fold this skill set's passive class-skill bonuses into `stats`.
    /// Called from the buff system each tick right after the stat reset (same
    /// slot as racial/class-attribute passives), so the bonuses stack with
    /// gear/buffs and need no persistence beyond the unlocked skill levels.
    ///
    /// Only the unlocked skills are walked (the skill map is small), and each
    /// is looked up in [`CLASS_SKILL_MODIFIERS`]; non-class /
    /// active-ability skills have no entry and are skipped.
    pub fn apply_class_passives(&self, stats: &mut Stats) {
        for (skill, level) in self.skills.iter() {
            if *level == 0 {
                continue;
            }
            if let Some(modifiers) = CLASS_SKILL_MODIFIERS.get(skill) {
                for (passive_stat, per_level) in modifiers.iter() {
                    passive_stat.apply(stats, per_level * f32::from(*level));
                }
            }
        }
    }

    /// BL-20: fold this skill set's passive feat bonuses into `stats`. Same
    /// tick slot as [`Self::apply_class_passives`] (called right after it in
    /// the buff system), so feats stack with class/gear/buff bonuses and need
    /// no persistence beyond the unlocked skill levels.
    ///
    /// Only the unlocked skills are walked (the skill map is small), and each
    /// is looked up in [`FEAT_MODIFIERS`]; non-feat / active-ability feats
    /// have no entry and are skipped.
    pub fn apply_feat_passives(&self, stats: &mut Stats) {
        for (skill, level) in self.skills.iter() {
            if *level == 0 {
                continue;
            }
            if let Some(modifiers) = FEAT_MODIFIERS.get(skill) {
                for (passive_stat, per_level) in modifiers.iter() {
                    passive_stat.apply(stats, per_level * f32::from(*level));
                }
            }
        }
    }
}

#[derive(Debug)]
pub enum SkillError {
    MissingSkill,
}

#[derive(Debug)]
pub enum SkillUnlockError {
    InsufficientSP,
    MissingPrerequisites,
    UnavailableSkillGroup,
    SkillAlreadyUnlocked,
    NoParentSkillTree,
}

#[derive(Debug)]
pub enum SpRewardError {
    InsufficientExp,
    UnavailableSkillGroup,
    Overflow,
}

#[derive(Debug, PartialEq, Eq, Clone, Copy, Deserialize, Serialize)]
pub enum SkillsPersistenceError {
    HashMismatch,
    DeserializationFailure,
    SpentExpMismatch,
    SkillsUnlockFailed,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub enum SkillPrerequisite {
    All(HashMap<Skill, u16>),
    Any(HashMap<Skill, u16>),
}

#[cfg(test)]
mod character_level_tests {
    use super::*;

    #[test]
    fn level_curve_boundaries() {
        assert_eq!(level_from_total_exp(0), 1);
        assert_eq!(level_from_total_exp(LEVEL_XP_BASE - 1), 1);
        assert_eq!(level_from_total_exp(LEVEL_XP_BASE), 2);
        assert_eq!(level_from_total_exp(u32::MAX), MAX_CHARACTER_LEVEL);
    }

    #[test]
    fn level_curve_is_monotonic() {
        let mut last = 0;
        for xp in (0..2_000_000u32).step_by(1000) {
            let level = level_from_total_exp(xp);
            assert!(level >= last, "level decreased at xp={xp}");
            last = level;
        }
    }

    #[test]
    fn default_skillset_is_level_one() {
        let skill_set = SkillSet::default();
        assert_eq!(skill_set.total_earned_exp(), 0);
        assert_eq!(skill_set.character_level(), 1);
    }

    #[test]
    fn earning_exp_raises_character_level() {
        let mut skill_set = SkillSet::default();
        // General pool exists on default skillsets
        skill_set.add_experience(SkillGroupKind::General, LEVEL_XP_BASE);
        assert_eq!(skill_set.total_earned_exp(), LEVEL_XP_BASE);
        assert_eq!(skill_set.character_level(), 2);
    }

    #[test]
    fn total_exp_for_level_inverts_level_from_total_exp() {
        for level in 1..=MAX_CHARACTER_LEVEL {
            let xp = total_exp_for_level(level);
            assert_eq!(
                level_from_total_exp(xp),
                level,
                "level_from_total_exp(total_exp_for_level({level})) mismatch"
            );
            if xp > 0 {
                assert_eq!(level_from_total_exp(xp - 1), level - 1);
            }
        }
    }

    #[test]
    fn set_level_sets_character_level() {
        for level in [1u16, 10, 52, 60] {
            let mut skill_set = SkillSet::default();
            skill_set.set_level(level);
            assert_eq!(
                skill_set.character_level(),
                level,
                "set_level({level}) should yield character_level()=={level}"
            );
        }
    }

    #[test]
    fn character_level_cache_matches_derived_after_experience_gain() {
        let mut skill_set = SkillSet::default();
        // Fresh skillset: cached field is the level-1 baseline, not the u16 default.
        assert_eq!(skill_set.character_level, 1);

        // Cross a few level boundaries via the public add-experience path.
        skill_set.add_experience(SkillGroupKind::General, LEVEL_XP_BASE * 9);
        assert!(
            skill_set.character_level > 1,
            "cache should have advanced past level 1"
        );
        // The cached field must equal a fresh derivation from lifetime XP.
        assert_eq!(
            skill_set.character_level,
            level_from_total_exp(skill_set.total_earned_exp()),
            "cached level desynced from total_earned_exp"
        );
        // And the public accessor must return the cached field.
        assert_eq!(skill_set.character_level(), skill_set.character_level);
    }

    #[test]
    fn character_level_cache_populated_on_load_from_database() {
        // Simulate a persisted General group carrying lifetime XP for ~level 4.
        let kind = SkillGroupKind::General;
        let mut sg = SkillGroup::new(kind);
        sg.earned_exp = LEVEL_XP_BASE * 9;
        sg.available_exp = sg.earned_exp;
        let mut groups = HashMap::new();
        groups.insert(kind, sg);

        let (skill_set, _err) = SkillSet::load_from_database(groups, HashMap::new());
        assert!(
            skill_set.character_level > 1,
            "load should have derived the persisted level, not left the default"
        );
        assert_eq!(
            skill_set.character_level,
            level_from_total_exp(skill_set.total_earned_exp()),
        );
    }

    #[test]
    fn character_level_cache_repopulated_after_deserialize() {
        // SkillSet is network-synced via serde; the cache is never sent over the
        // wire (#[serde(skip)]) and must be rebuilt from XP on deserialize.
        let mut skill_set = SkillSet::default();
        skill_set.add_experience(SkillGroupKind::General, LEVEL_XP_BASE * 9);
        let expected = skill_set.character_level;
        assert!(expected > 1);

        let encoded = ron::to_string(&skill_set).expect("serialize");
        // The cache must not ride along the wire/disk — it's rebuilt from XP.
        assert!(
            !encoded.contains("character_level"),
            "derived level leaked into the serialized form"
        );
        let restored: SkillSet = ron::from_str(&encoded).expect("deserialize");
        assert_eq!(
            restored.character_level, expected,
            "cache not rebuilt on deserialize"
        );
        assert_eq!(restored.character_level(), expected);
    }

    #[test]
    fn set_level_clamps_out_of_range() {
        let mut over = SkillSet::default();
        over.set_level(100);
        assert_eq!(over.character_level(), MAX_CHARACTER_LEVEL);

        let mut under = SkillSet::default();
        under.set_level(0);
        assert_eq!(under.character_level(), 1);
    }

    #[test]
    fn set_level_downlevel_never_corrupts_spent_exp() {
        // Simulate a General group with spent exp (earned 5000, available 1000 →
        // spent 4000), as if skill points had been bought.
        let mut ss = SkillSet::default();
        {
            let sg = ss
                .skill_groups
                .get_mut(&SkillGroupKind::General)
                .expect("General always exists");
            sg.earned_exp = 5000;
            sg.available_exp = 1000;
        }
        assert_eq!(ss.skill_groups[&SkillGroupKind::General].spent_exp(), 4000);

        // Down-level to 1 (target total exp 0): earned must floor at spent, not
        // drop below it (which would silently shrink spent_exp → respec on relog).
        ss.set_level(1);
        let sg = &ss.skill_groups[&SkillGroupKind::General];
        assert!(
            sg.earned_exp >= sg.available_exp,
            "earned >= available invariant"
        );
        assert_eq!(
            sg.spent_exp(),
            4000,
            "spent exp preserved (no silent respec)"
        );
        assert_eq!(sg.earned_exp, 4000, "floored at spent, never below");
    }
}

#[cfg(test)]
mod class_tree_tests {
    use super::*;

    #[test]
    fn class_skill_groups_have_defs_and_stable_hashes() {
        // BL-06 proof slice: these 4 trees are populated; the other 10 are still
        // empty stubs until they are authored in a later phase.
        const POPULATED: [ClassKind; 4] = [
            ClassKind::Warrior,
            ClassKind::Mage,
            ClassKind::Cleric,
            ClassKind::Rogue,
        ];
        for class in ClassKind::PLAYABLE {
            let group = SkillGroupKind::Class(class);
            assert!(
                SKILL_GROUP_DEFS.contains_key(&group),
                "missing manifest entry: {group:?}"
            );
            assert!(
                SKILL_GROUP_HASHES.contains_key(&group),
                "missing hash: {group:?}"
            );
            if POPULATED.contains(&class) {
                assert!(
                    group.total_skill_point_cost() > 0,
                    "{group:?} proof-slice tree should have purchasable skills"
                );
            } else {
                // Stub trees stay empty until authored (P5+).
                assert_eq!(
                    group.total_skill_point_cost(),
                    0,
                    "{group:?} should be empty"
                );
            }
        }
    }

    // Class-gated abilities (e.g. Mage-only Shatterburst) rely on
    // has_skill(UnlockGroup(Class(..))) reflecting membership.
    #[test]
    fn class_membership_is_detectable_via_has_skill() {
        let mut mage = SkillSet::default();
        mage.unlock_skill_group(SkillGroupKind::Class(ClassKind::Mage));
        assert!(mage.has_skill(Skill::UnlockGroup(SkillGroupKind::Class(ClassKind::Mage))));
        assert!(!mage.has_skill(Skill::UnlockGroup(SkillGroupKind::Class(ClassKind::Cleric))));
    }

    // A character loaded from the DB (skill_groups restored, skills map lacking the
    // class UnlockGroup — e.g. saved before the seeding) still registers
    // membership.
    #[test]
    fn loaded_class_member_keeps_unlock_skill() {
        let kind = SkillGroupKind::Class(ClassKind::Cleric);
        let mut groups = HashMap::new();
        groups.insert(kind, SkillGroup::new(kind));
        let (skillset, _err) = SkillSet::load_from_database(groups, HashMap::new());
        assert!(skillset.has_skill(Skill::UnlockGroup(kind)));
    }
}
