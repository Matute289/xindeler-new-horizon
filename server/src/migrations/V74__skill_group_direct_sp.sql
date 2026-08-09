-- Skill points granted directly via `SkillSet::grant_skill_point` (e.g.
-- level-milestone feats) bypass the exp economy entirely and were never
-- persisted, so they were silently lost on every character reload.
-- `earned_exp`/`available_exp` still only track the normal exp-driven
-- economy; these two columns separately track the exp-independent grants so
-- they survive a save/load round-trip without perturbing
-- `total_earned_exp()` (and therefore the derived character level, which
-- must stay driven by real gameplay exp only).
ALTER TABLE "skill_group" ADD COLUMN direct_earned_sp INTEGER NOT NULL DEFAULT 0;
ALTER TABLE "skill_group" ADD COLUMN direct_available_sp INTEGER NOT NULL DEFAULT 0;
