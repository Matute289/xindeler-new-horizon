use super::{
    BLACK, CRITICAL_HP_COLOR, HP_COLOR, Position, PositionSpecifier, Show, TEXT_COLOR,
    UI_HIGHLIGHT_0, UI_MAIN, XP_COLOR,
    img_ids::{Imgs, ImgsRot},
    item_imgs::{ItemImgs, animate_by_pulse},
};
use crate::{
    GlobalState,
    game_input::GameInput,
    hud::{
        self,
        slots::{AbilitySlot, SlotManager},
        util,
    },
    ui::{
        ImageFrame, Tooltip, TooltipManager, Tooltipable,
        fonts::Fonts,
        slot::{ContentSize, SlotMaker},
    },
    window::{LastInput, MenuInput},
};
use client::{self, Client};
use common::{
    combat,
    comp::{
        self, Buffs, CharacterClass, CharacterState, ClassKind, Combo, DerivedStats, Energy,
        Health, Inventory, Poise, Stance, Stats,
        ability::{
            Ability, AbilityPool, ActiveAbilities, AuxiliaryAbility, BASE_ABILITY_LIMIT, SpellGate,
        },
        inventory::{
            item::{
                ItemI18n, ItemKind,
                item_key::ItemKey,
                tool::{ToolKind, WeaponRole},
            },
            slot::EquipSlot,
        },
        pact::PactStanding,
        skills::{
            self, AxeSkill, BowSkill, ClericSkill, ClimbSkill, HammerSkill, MageSkill, MiningSkill,
            RogueSkill, SKILL_MODIFIERS, SceptreSkill, Skill, SwimSkill, SwordSkill, WarriorSkill,
        },
        skillset::{
            CLASS_SKILL_MODIFIERS, SKILL_GROUP_DEFS, SKILL_PREREQUISITES, SkillGroupKind,
            SkillPrerequisite, SkillSet,
        },
    },
    resources::BattleMode,
    uid::Uid,
};
use conrod_core::{
    Borderable, Color, Colorable, Labelable, Positionable, Sizeable, UiCell, Widget, WidgetCommon,
    color, image,
    position::Relative,
    widget::{self, Button, Image, Rectangle, State, Text},
    widget_ids,
};
use i18n::Localization;
use specs::WorldExt;
use std::borrow::Cow;
use strum::{EnumIter, IntoEnumIterator};
use vek::*;
const ART_SIZE: [f64; 2] = [320.0, 320.0];

/// Ability-browse grid layout (`DiarySection::AbilitySelection`): rows per
/// column, and abilities per page (2 columns). Shared between the render
/// code and the gamepad/keyboard grid-nav math in `Widget::update` so the
/// two can't drift out of sync.
const ABILITY_GRID_ROWS_PER_COL: usize = 6;
const ABILITIES_PER_PAGE: usize = ABILITY_GRID_ROWS_PER_COL * 2;
/// Spell-browse grid layout (`DiarySection::Spells`): rows per column
/// (reduced from the ability tab's 6 to leave room for the per-source
/// mastery header at the top of each page), and spells per page (2
/// columns). Shared between the render code and the grid-nav math in
/// `Widget::update`.
const SPELL_GRID_ROWS_PER_COL: usize = 5;
const SPELLS_PER_PAGE: usize = SPELL_GRID_ROWS_PER_COL * 2;

widget_ids! {
    pub struct Ids {
        frame,
        bg,
        icon,
        close,
        title,
        content_align,
        section_imgs[],
        section_btns[],
        // Skill tree stuffs
        exp_bar_bg,
        exp_bar_frame,
        exp_bar_content_align,
        exp_bar_content,
        exp_bar_rank,
        exp_bar_txt,
        active_bar_checkbox,
        active_bar_checkbox_label,
        tree_title_txt,
        lock_imgs[],
        available_pts_txt,
        weapon_imgs[],
        weapon_btns[],
        skills_top_l_align,
        skills_top_r_align,
        skills_bot_l_align,
        skills_bot_r_align,
        skills_top_l[],
        skills_top_r[],
        skills_bot_l[],
        skills_bot_r[],
        skills[],
        skill_lock_imgs[],
        sword_bg,
        sword_stance_cleaving_text,
        sword_stance_agile_text,
        sword_stance_crippling_text,
        sword_stance_heavy_text,
        sword_stance_defensive_text,
        sword_stance_cleaving_shadow,
        sword_stance_agile_shadow,
        sword_stance_crippling_shadow,
        sword_stance_heavy_shadow,
        sword_stance_defensive_shadow,
        sword_stance_left_align,
        sword_stance_right_align,
        axe_bg,
        hammer_bg,
        bow_bg,
        staff_bg,
        staff_render,
        skill_staff_basic_0,
        skill_staff_basic_1,
        skill_staff_basic_2,
        skill_staff_basic_3,
        skill_staff_beam_0,
        skill_staff_beam_1,
        skill_staff_beam_2,
        skill_staff_beam_3,
        skill_staff_beam_4,
        skill_staff_shockwave_0,
        skill_staff_shockwave_1,
        skill_staff_shockwave_2,
        skill_staff_shockwave_3,
        skill_staff_shockwave_4,
        skill_staff_napalm_strike,
        skill_staff_flame_cloak,
        skill_staff_fire_dash,
        skill_staff_fire_breath,
        skill_staff_pyroclasm,
        sceptre_render,
        skill_sceptre_lifesteal_0,
        skill_sceptre_lifesteal_1,
        skill_sceptre_lifesteal_2,
        skill_sceptre_lifesteal_3,
        skill_sceptre_lifesteal_4,
        skill_sceptre_heal_0,
        skill_sceptre_heal_1,
        skill_sceptre_heal_2,
        skill_sceptre_heal_3,
        skill_sceptre_heal_4,
        skill_sceptre_aura_0,
        skill_sceptre_aura_1,
        skill_sceptre_aura_2,
        skill_sceptre_aura_3,
        skill_sceptre_aura_4,
        pick_render,
        skill_pick_m1,
        skill_pick_m1_0,
        skill_pick_m1_1,
        skill_pick_m1_2,
        general_combat_render_0,
        general_combat_render_1,
        skill_general_tree_0,
        skill_general_tree_1,
        skill_general_tree_2,
        skill_general_tree_3,
        skill_general_tree_4,
        skill_general_tree_5,
        skill_general_tree_6,
        skill_general_climb_0,
        skill_general_climb_1,
        skill_general_climb_2,
        skill_general_swim_0,
        skill_general_swim_1,
        sword_path_overlay,
        // Ability selection
        spellbook_art,
        sb_page_left_align,
        sb_page_right_align,
        spellbook_skills_bg,
        ability_page_left,
        ability_page_right,
        active_abilities[],
        active_abilities_keys[],
        abilities[],
        ability_frames[],
        abilities_dual[],
        ability_titles[],
        ability_descs[],
        // Xindeler: spell selection. Deliberately a separate set of ids from the
        // ability tab's rather than shared: the two tabs are mutually exclusive
        // but have different parents, and reusing ids would reparent widgets.
        spells_art,
        sp_page_left_align,
        sp_page_right_align,
        spells_skills_bg,
        spell_page_left,
        spell_page_right,
        spell_active_abilities[],
        spell_active_abilities_keys[],
        spell_slots[],
        spell_locked_slots[],
        spell_locks[],
        spell_frames[],
        spell_titles[],
        spell_metas[],
        spell_reqs[],
        spell_empty_txt,
        // Xindeler: per-source mastery header. `spell_mastery_labels` holds
        // 5 entries (Arcane first, then the 4 bar sources); the bar arrays
        // hold 4 (Arcane is known by default and never drawn as a bar).
        spell_mastery_labels[],
        spell_mastery_bar_bg[],
        spell_mastery_bar_content[],
        // Stats
        stat_names[],
        stat_values[],
        // Recipes
        recipe_groups[],
        // BL-06 P3a: generic class skill-tree tab
        class_tree_align,
        class_tree_empty_txt,
        class_skills[],
        class_skill_lock_imgs[],
        // Multiclass: in-tab primary/secondary toggle + future-levels routing
        class_toggle_btn,
        class_toggle_label,
        future_levels_checkbox,
        future_levels_label,
    }
}

#[derive(WidgetCommon)]
pub struct Diary<'a> {
    show: &'a Show,
    client: &'a Client,
    global_state: &'a GlobalState,
    skill_set: &'a SkillSet,
    active_abilities: &'a ActiveAbilities,
    ability_pool: Option<&'a AbilityPool>,
    inventory: &'a Inventory,
    char_state: &'a CharacterState,
    health: &'a Health,
    energy: &'a Energy,
    poise: &'a Poise,
    uid: &'a Uid,
    imgs: &'a Imgs,
    item_imgs: &'a ItemImgs,
    fonts: &'a Fonts,
    localized_strings: &'a Localization,
    item_i18n: &'a ItemI18n,
    rot_imgs: &'a ImgsRot,
    tooltip_manager: &'a mut TooltipManager,
    slot_manager: &'a mut SlotManager,
    pulse: f32,
    stance: Option<&'a Stance>,
    combo: Option<&'a Combo>,
    stats: Option<&'a Stats>,
    buffs: Option<&'a Buffs>,
    character_class: Option<&'a CharacterClass>,
    spell_mastery: Option<&'a comp::SpellMastery>,
    pact: Option<&'a comp::Pact>,
    menu_events: &'a [MenuInput],

    #[conrod(common_builder)]
    common: widget::CommonBuilder,
    created_btns_top_l: usize,
    created_btns_top_r: usize,
    created_btns_bot_l: usize,
    created_btns_bot_r: usize,
}

pub struct DiaryShow {
    pub skilltreetab: SelectedSkillTree,
    pub section: DiarySection,
}

impl Default for DiaryShow {
    fn default() -> Self {
        Self {
            skilltreetab: SelectedSkillTree::General,
            section: DiarySection::SkillTrees,
        }
    }
}

#[expect(clippy::too_many_arguments)]
impl<'a> Diary<'a> {
    pub fn new(
        show: &'a Show,
        client: &'a Client,
        global_state: &'a GlobalState,
        skill_set: &'a SkillSet,
        active_abilities: &'a ActiveAbilities,
        ability_pool: Option<&'a AbilityPool>,
        inventory: &'a Inventory,
        char_state: &'a CharacterState,
        health: &'a Health,
        energy: &'a Energy,
        poise: &'a Poise,
        uid: &'a Uid,
        imgs: &'a Imgs,
        item_imgs: &'a ItemImgs,
        fonts: &'a Fonts,
        localized_strings: &'a Localization,
        item_i18n: &'a ItemI18n,
        rot_imgs: &'a ImgsRot,
        tooltip_manager: &'a mut TooltipManager,
        slot_manager: &'a mut SlotManager,
        pulse: f32,
        stance: Option<&'a Stance>,
        combo: Option<&'a Combo>,
        stats: Option<&'a Stats>,
        buffs: Option<&'a Buffs>,
        character_class: Option<&'a CharacterClass>,
        spell_mastery: Option<&'a comp::SpellMastery>,
        pact: Option<&'a comp::Pact>,
        menu_events: &'a [MenuInput],
    ) -> Self {
        Self {
            show,
            client,
            global_state,
            skill_set,
            active_abilities,
            ability_pool,
            inventory,
            char_state,
            health,
            energy,
            poise,
            uid,
            imgs,
            item_imgs,
            fonts,
            localized_strings,
            item_i18n,
            rot_imgs,
            tooltip_manager,
            slot_manager,
            pulse,
            stance,
            combo,
            stats,
            buffs,
            character_class,
            spell_mastery,
            pact,
            menu_events,
            common: widget::CommonBuilder::default(),
            created_btns_top_l: 0,
            created_btns_top_r: 0,
            created_btns_bot_l: 0,
            created_btns_bot_r: 0,
        }
    }
}

pub type SelectedSkillTree = SkillGroupKind;

pub enum Event {
    Close,
    ChangeSkillTree(SelectedSkillTree),
    UnlockSkill(Skill),
    ChangeSection(DiarySection),
    SelectExpBar(Option<SkillGroupKind>),
    SetFutureLevelsToSecondary(bool),
}

// Possible future sections: Bestiary ("Pokedex" of fought enemies), Weapon and
// armour catalogue, Achievements...
#[derive(EnumIter, PartialEq, Eq)]
pub enum DiarySection {
    SkillTrees,
    AbilitySelection,
    /// Xindeler: the class+level-gated spell list.
    Spells,
    Character,
    Recipes,
}

impl DiarySection {
    fn title_key(&self) -> &'static str {
        match self {
            DiarySection::SkillTrees => "hud-diary-sections-skill_trees-title",
            DiarySection::AbilitySelection => "hud-diary-sections-abilities-title",
            DiarySection::Spells => "hud-diary-sections-spells-title",
            DiarySection::Character => "hud-diary-sections-character-title",
            DiarySection::Recipes => "hud-diary-sections-recipes-title",
        }
    }
}

// Represents the SkillGroupKind items
// that have a skill tree in the diary
#[derive(EnumIter, PartialEq, Eq)]
pub enum DiarySkillTree {
    General,
    Sword,
    Axe,
    Hammer,
    Bow,
    Staff,
    // The martial Staff's own tab, separate from `Staff` above (the caster
    // fire tree) since the two are distinct `SkillGroupKind`s sharing a
    // `ToolKind`.
    StaffMartial,
    Sceptre,
    Pick,
    // BL-06 P3a: generic class skill-tree tab. The live ClassKind is
    // determined from the skill set at render time; this variant carries no
    // data because `DiarySkillTree` must be `EnumIter` + static.
    Class,
}

impl DiarySkillTree {
    fn title_key(&self) -> &'static str {
        match self {
            DiarySkillTree::General => "hud-skill_tree-general",
            DiarySkillTree::Sword => "hud-skill_tree-sword",
            DiarySkillTree::Axe => "hud-skill_tree-axe",
            DiarySkillTree::Hammer => "hud-skill_tree-hammer",
            DiarySkillTree::Bow => "hud-skill_tree-bow",
            DiarySkillTree::Staff => "hud-skill_tree-staff",
            DiarySkillTree::StaffMartial => "hud-skill_tree-staff_martial",
            DiarySkillTree::Sceptre => "hud-skill_tree-sceptre",
            DiarySkillTree::Pick => "hud-skill_tree-mining",
            DiarySkillTree::Class => "hud-skill_tree-class",
        }
    }

    fn to_skill_group(&self) -> SkillGroupKind {
        match self {
            DiarySkillTree::General => SkillGroupKind::General,
            DiarySkillTree::Sword => SkillGroupKind::Weapon(ToolKind::Sword),
            DiarySkillTree::Axe => SkillGroupKind::Weapon(ToolKind::Axe),
            DiarySkillTree::Hammer => SkillGroupKind::Weapon(ToolKind::Hammer),
            DiarySkillTree::Bow => SkillGroupKind::Weapon(ToolKind::Bow),
            DiarySkillTree::Staff => SkillGroupKind::Weapon(ToolKind::Staff),
            DiarySkillTree::StaffMartial => {
                SkillGroupKind::WeaponRoled(ToolKind::Staff, WeaponRole::Martial)
            },
            DiarySkillTree::Sceptre => SkillGroupKind::Weapon(ToolKind::Sceptre),
            DiarySkillTree::Pick => SkillGroupKind::Weapon(ToolKind::Pick),
            // For the Class variant the live group is resolved at render time via
            // `selected_class_group()`; `General` here is a never-reached
            // placeholder that satisfies the exhaustive match.
            DiarySkillTree::Class => SkillGroupKind::General,
        }
    }
}

pub struct DiaryState {
    ids: Ids,
    ability_page: usize,
    /// Xindeler: paging state of the spell tab, kept separate from
    /// `ability_page` so the two tabs page independently.
    spell_page: usize,
    recipe_page: usize,
    // Gamepad/keyboard menu navigation (mirrors bag.rs's `active_content` LocalFocus
    // cycling). 0 = section list, 1 = active-abilities row (AbilitySelection/Spells
    // only), 2 = ability-browse grid (AbilitySelection/Spells only). SkillTrees/
    // Character/Recipes sections only use area 0 — their own content nav is
    // out of scope for this pass (mouse-emulation click-through still works).
    active_content: usize,
    active_section_index: usize,
    /// Index into the current page's active-abilities row (area 1).
    active_row_index: usize,
    /// Index into the current page's ability-browse grid (area 2), shared
    /// between the AbilitySelection and Spells sections since only one is
    /// ever visible at a time.
    active_grid_index: usize,
}

impl Widget for Diary<'_> {
    type Event = Vec<Event>;
    type State = DiaryState;
    type Style = ();

    fn init_state(&self, id_gen: widget::id::Generator) -> Self::State {
        DiaryState {
            ids: Ids::new(id_gen),
            ability_page: 0,
            spell_page: 0,
            recipe_page: 0,
            active_content: 0,
            active_section_index: 0,
            active_row_index: 0,
            active_grid_index: 0,
        }
    }

    fn style(&self) -> Self::Style {}

    fn update(mut self, args: widget::UpdateArgs<Self>) -> Self::Event {
        common_base::prof_span!("Diary::update");
        let widget::UpdateArgs { state, ui, .. } = args;
        let mut events = Vec::new();

        // MENU INPUTS: diary navigation, mirroring the SlotGrid/menu_events pattern
        // the bag already uses (see hud/slot_grid.rs, hud/bag.rs).
        //
        // Local focus areas (LocalFocus cycles through them):
        // - Area 0, section list: Up/Down highlights a section (SkillTrees,
        //   AbilitySelection, Spells, Character, Recipes), Apply switches to it.
        // - Area 1, active-abilities row: only meaningful in the AbilitySelection/
        //   Spells sections (a no-op elsewhere). Left/Right highlights a slot in the
        //   fixed-size action-bar row, Apply selects it via the shared SlotManager.
        // - Area 2, ability-browse grid: only meaningful in the AbilitySelection/
        //   Spells sections. Up/Down/Left/Right highlights a slot in the current page's
        //   2-column grid, Apply selects it. PageUp/PageDown turns the page (works
        //   regardless of local focus, while one of these two sections is showing) —
        //   the existing per-section render code already clamps the page index to the
        //   real page count each frame, so this need not know the list length up front.
        //
        // SkillTrees/Character/Recipes sections only get area 0 (section-switching)
        // navigation in this pass — their own content (a skill-point node graph, a
        // stats readout, and a recipe list respectively) is out of scope here; the
        // existing mouse-emulation fallback still reaches them.
        //
        // Back closes the diary, same as the X button.
        let sections_len = DiarySection::iter().count();
        let last_input = self.global_state.window.last_input();
        let menu_active = matches!(last_input, LastInput::Keyboard | LastInput::Controller);
        let (grid_rows_per_col, grid_per_page) = match self.show.diary_fields.section {
            DiarySection::Spells => (SPELL_GRID_ROWS_PER_COL, SPELLS_PER_PAGE),
            _ => (ABILITY_GRID_ROWS_PER_COL, ABILITIES_PER_PAGE),
        };
        let mut apply_pressed = false;
        let mut ability_row_apply = false;
        let mut ability_grid_apply = false;
        for key in self.menu_events {
            match *key {
                MenuInput::Back => events.push(Event::Close),
                MenuInput::LocalFocus => state.update(|s| {
                    s.active_content = (s.active_content + 1) % 3;
                }),
                MenuInput::Up if state.active_content == 0 => state.update(|s| {
                    s.active_section_index = s.active_section_index.saturating_sub(1);
                }),
                MenuInput::Down if state.active_content == 0 && sections_len > 0 => {
                    state.update(|s| {
                        s.active_section_index = (s.active_section_index + 1).min(sections_len - 1);
                    });
                },
                MenuInput::Apply if state.active_content == 0 => apply_pressed = true,
                MenuInput::Left if state.active_content == 1 => state.update(|s| {
                    s.active_row_index = s.active_row_index.saturating_sub(1);
                }),
                MenuInput::Right if state.active_content == 1 => state.update(|s| {
                    s.active_row_index = (s.active_row_index + 1).min(BASE_ABILITY_LIMIT - 1);
                }),
                MenuInput::Apply if state.active_content == 1 => ability_row_apply = true,
                MenuInput::Left if state.active_content == 2 => state.update(|s| {
                    if s.active_grid_index >= grid_rows_per_col {
                        s.active_grid_index -= grid_rows_per_col;
                    }
                }),
                MenuInput::Right if state.active_content == 2 => state.update(|s| {
                    if s.active_grid_index + grid_rows_per_col < grid_per_page {
                        s.active_grid_index += grid_rows_per_col;
                    }
                }),
                MenuInput::Up if state.active_content == 2 => state.update(|s| {
                    if s.active_grid_index % grid_rows_per_col > 0 {
                        s.active_grid_index -= 1;
                    }
                }),
                MenuInput::Down if state.active_content == 2 => state.update(|s| {
                    if s.active_grid_index % grid_rows_per_col + 1 < grid_rows_per_col {
                        s.active_grid_index += 1;
                    }
                }),
                MenuInput::Apply if state.active_content == 2 => ability_grid_apply = true,
                MenuInput::PageUp => match self.show.diary_fields.section {
                    DiarySection::Spells => state.update(|s| {
                        s.spell_page = s.spell_page.saturating_sub(1);
                    }),
                    DiarySection::AbilitySelection => state.update(|s| {
                        s.ability_page = s.ability_page.saturating_sub(1);
                    }),
                    _ => {},
                },
                MenuInput::PageDown => match self.show.diary_fields.section {
                    DiarySection::Spells => state.update(|s| {
                        s.spell_page += 1;
                    }),
                    DiarySection::AbilitySelection => state.update(|s| {
                        s.ability_page += 1;
                    }),
                    _ => {},
                },
                _ => {},
            }
        }
        let active_section_index = state
            .active_section_index
            .min(sections_len.saturating_sub(1));

        // Tooltips
        let diary_tooltip = Tooltip::new({
            // Edge images [t, b, r, l]
            // Corner images [tr, tl, br, bl]
            let edge = &self.rot_imgs.tt_side;
            let corner = &self.rot_imgs.tt_corner;
            ImageFrame::new(
                [edge.cw180, edge.none, edge.cw270, edge.cw90],
                [corner.none, corner.cw270, corner.cw90, corner.cw180],
                Color::Rgba(0.08, 0.07, 0.04, 1.0),
                5.0,
            )
        })
        .title_font_size(self.fonts.cyri.scale(15))
        .parent(ui.window)
        .desc_font_size(self.fonts.cyri.scale(12))
        .font_id(self.fonts.cyri.conrod_id)
        .desc_text_color(TEXT_COLOR);

        //Animation timer Frame
        let frame_ani = (self.pulse * 4.0/* speed factor */).cos() * 0.5 + 0.8;

        Image::new(self.imgs.diary_bg)
            .w_h(1202.0, 886.0)
            .mid_top_with_margin_on(ui.window, 5.0)
            .color(Some(UI_MAIN))
            .set(state.ids.bg, ui);

        Image::new(self.imgs.diary_frame)
            .w_h(1202.0, 886.0)
            .middle_of(state.ids.bg)
            .color(Some(UI_HIGHLIGHT_0))
            .set(state.ids.frame, ui);

        // Icon
        Image::new(self.imgs.spellbook_button)
            .w_h(30.0, 27.0)
            .top_left_with_margins_on(state.ids.frame, 8.0, 8.0)
            .set(state.ids.icon, ui);

        // X-Button
        if Button::image(self.imgs.close_btn)
            .w_h(24.0, 25.0)
            .hover_image(self.imgs.close_btn_hover)
            .press_image(self.imgs.close_btn_press)
            .top_right_with_margins_on(state.ids.frame, 0.0, 0.0)
            .set(state.ids.close, ui)
            .was_clicked()
        {
            events.push(Event::Close);
        }

        // Title
        Text::new(&self.localized_strings.get_msg("hud-diary"))
            .mid_top_with_margin_on(state.ids.frame, 3.0)
            .font_id(self.fonts.cyri.conrod_id)
            .font_size(self.fonts.cyri.scale(29))
            .color(TEXT_COLOR)
            .set(state.ids.title, ui);

        // Content Alignment
        Rectangle::fill_with([599.0 * 2.0, 419.0 * 2.0], color::TRANSPARENT)
            .mid_top_with_margin_on(state.ids.frame, 46.0)
            .set(state.ids.content_align, ui);

        // Contents
        // Section buttons
        let sel_section = &self.show.diary_fields.section;

        let sections_len = DiarySection::iter().enumerate().len();

        // Update len
        state.update(|s| {
            s.ids
                .section_imgs
                .resize(sections_len, &mut ui.widget_id_generator())
        });
        state.update(|s| {
            s.ids
                .section_btns
                .resize(sections_len, &mut ui.widget_id_generator())
        });

        for (i, section) in DiarySection::iter().enumerate() {
            let section_name = self.localized_strings.get_msg(section.title_key());

            let btn_img = {
                let img = match section {
                    DiarySection::AbilitySelection => self.imgs.spellbook_ico,
                    DiarySection::Spells => self.imgs.spellbook_ico0,
                    DiarySection::SkillTrees => self.imgs.skilltree_ico,
                    DiarySection::Character => self.imgs.stats_ico,
                    DiarySection::Recipes => self.imgs.crafting_ico,
                };

                if i == 0 {
                    Image::new(img).top_left_with_margins_on(state.ids.content_align, 0.0, -50.0)
                } else {
                    Image::new(img).down_from(state.ids.section_btns[i - 1], 5.0)
                }
            };
            btn_img.w_h(50.0, 50.0).set(state.ids.section_imgs[i], ui);
            // Section Buttons
            let border_image = if section == *sel_section {
                self.imgs.wpn_icon_border_pressed
            } else {
                self.imgs.wpn_icon_border
            };

            let hover_image = if section == *sel_section {
                self.imgs.wpn_icon_border_pressed
            } else {
                self.imgs.wpn_icon_border_mo
            };

            let press_image = if section == *sel_section {
                self.imgs.wpn_icon_border_pressed
            } else {
                self.imgs.wpn_icon_border_press
            };
            let section_menu_highlighted =
                menu_active && state.active_content == 0 && i == active_section_index;
            let section_buttons = Button::image(border_image)
                .w_h(50.0, 50.0)
                .hover_image(hover_image)
                .press_image(press_image)
                .middle_of(state.ids.section_imgs[i])
                .border(if section_menu_highlighted { 2.0 } else { 0.0 })
                .border_color(if section_menu_highlighted {
                    color::YELLOW
                } else {
                    color::TRANSPARENT
                })
                .with_tooltip(
                    self.tooltip_manager,
                    &section_name,
                    "",
                    &diary_tooltip,
                    TEXT_COLOR,
                )
                .set(state.ids.section_btns[i], ui);
            if section_buttons.was_clicked() || (apply_pressed && section_menu_highlighted) {
                events.push(Event::ChangeSection(section))
            }
        }
        match self.show.diary_fields.section {
            DiarySection::SkillTrees => {
                // Skill Trees
                let sel_tab = &self.show.diary_fields.skilltreetab;

                let skill_trees_len = DiarySkillTree::iter().enumerate().len();

                // Skill Tree Selection
                state.update(|s| {
                    s.ids
                        .weapon_btns
                        .resize(skill_trees_len, &mut ui.widget_id_generator())
                });
                state.update(|s| {
                    s.ids
                        .weapon_imgs
                        .resize(skill_trees_len, &mut ui.widget_id_generator())
                });
                state.update(|s| {
                    s.ids
                        .lock_imgs
                        .resize(skill_trees_len, &mut ui.widget_id_generator())
                });

                // Resolve the live class group once (None = Adventurer /
                // unclassed).
                let class_group = self.selected_class_group();

                // Draw skillgroup tab's icons
                for (i, skill_tree) in DiarySkillTree::iter().enumerate() {
                    // The Class tab is only shown when the character has a class
                    // skill group. Skip entirely otherwise so the tab doesn't
                    // appear as a locked "General" placeholder.
                    if skill_tree == DiarySkillTree::Class && class_group.is_none() {
                        continue;
                    }

                    let skill_tree_name = self.localized_strings.get_msg(skill_tree.title_key());
                    // For the Class variant, use the live group; for all others
                    // use the static mapping.
                    let skill_group = if skill_tree == DiarySkillTree::Class {
                        // Safety: we skipped above when class_group is None.
                        class_group.unwrap()
                    } else {
                        skill_tree.to_skill_group()
                    };

                    // Check if we have this skill tree unlocked
                    let locked = !self.skill_set.skill_group_accessible(skill_group);

                    // Weapon button image
                    let btn_img = {
                        let img = match skill_tree {
                            DiarySkillTree::General => self.imgs.swords_crossed,
                            DiarySkillTree::Sword => self.imgs.sword,
                            DiarySkillTree::Axe => self.imgs.axe,
                            DiarySkillTree::Hammer => self.imgs.hammer,
                            DiarySkillTree::Bow => self.imgs.bow,
                            DiarySkillTree::Staff => self.imgs.staff,
                            // Reuses the caster Staff tab's icon (no bespoke
                            // martial-staff art yet); tooltip text still
                            // disambiguates the two tabs.
                            DiarySkillTree::StaffMartial => self.imgs.staff,
                            DiarySkillTree::Sceptre => self.imgs.sceptre,
                            DiarySkillTree::Pick => self.imgs.mining,
                            // Use the skilltree icon for the class tab (a
                            // generic skill-tree image already in the atlas).
                            DiarySkillTree::Class => self.imgs.skilltree_ico,
                        };

                        if i == 0 {
                            Image::new(img).top_left_with_margins_on(
                                state.ids.content_align,
                                10.0,
                                5.0,
                            )
                        } else {
                            Image::new(img).down_from(state.ids.weapon_btns[i - 1], 5.0)
                        }
                    };
                    btn_img.w_h(50.0, 50.0).set(state.ids.weapon_imgs[i], ui);

                    // Lock Image
                    if locked {
                        Image::new(self.imgs.lock)
                            .w_h(50.0, 50.0)
                            .middle_of(state.ids.weapon_imgs[i])
                            .graphics_for(state.ids.weapon_imgs[i])
                            .color(Some(Color::Rgba(1.0, 1.0, 1.0, 0.8)))
                            .set(state.ids.lock_imgs[i], ui);
                    }

                    // Weapon icons
                    let have_points = {
                        let available = self.skill_set.available_sp(skill_group);

                        let earned = self.skill_set.earned_sp(skill_group);
                        let total_cost = skill_group.total_skill_point_cost();

                        available > 0 && (earned - available) < total_cost
                    };

                    let border_image = if skill_group == *sel_tab || have_points {
                        self.imgs.wpn_icon_border_pressed
                    } else {
                        self.imgs.wpn_icon_border
                    };

                    let hover_image = if skill_group == *sel_tab {
                        self.imgs.wpn_icon_border_pressed
                    } else {
                        self.imgs.wpn_icon_border_mo
                    };

                    let press_image = if skill_group == *sel_tab {
                        self.imgs.wpn_icon_border_pressed
                    } else {
                        self.imgs.wpn_icon_border_press
                    };

                    let color = if skill_group != *sel_tab && have_points {
                        Color::Rgba(0.92, 0.76, 0.0, frame_ani)
                    } else {
                        TEXT_COLOR
                    };

                    let tooltip_txt = if locked {
                        self.localized_strings.get_msg("hud-skill-not_unlocked")
                    } else {
                        Cow::Borrowed("")
                    };

                    let wpn_button = Button::image(border_image)
                        .w_h(50.0, 50.0)
                        .hover_image(hover_image)
                        .press_image(press_image)
                        .middle_of(state.ids.weapon_imgs[i])
                        .image_color(color)
                        .with_tooltip(
                            self.tooltip_manager,
                            &skill_tree_name,
                            &tooltip_txt,
                            &diary_tooltip,
                            TEXT_COLOR,
                        )
                        .set(state.ids.weapon_btns[i], ui);
                    if wpn_button.was_clicked() {
                        events.push(Event::ChangeSkillTree(skill_group))
                    }
                }

                // Multiclass: in-tab toggle between the primary's and the
                // secondary's skill tree, plus the set-and-forget "future
                // levels" routing preference. Both only ever shown while the
                // Class tab is the active tab.
                if let (Some(character_class), Some(class_group)) =
                    (self.character_class, class_group)
                    && character_class.is_multiclass()
                    && *sel_tab == class_group
                {
                    let other_class =
                        if class_group == SkillGroupKind::Class(character_class.primary) {
                            character_class.secondary
                        } else {
                            Some(character_class.primary)
                        };
                    if let Some(other_class) = other_class {
                        let other_class_key =
                            format!("char_selection-class_{}", other_class.keyword());
                        let other_class_name = self
                            .localized_strings
                            .get_msg(&other_class_key)
                            .into_owned();
                        let switch_label = self.localized_strings.get_msg_ctx(
                            "hud-skill_tree-multiclass_switch_to",
                            &i18n::fluent_args! {
                                "class" => other_class_name.clone(),
                            },
                        );

                        if Button::image(self.imgs.wpn_icon_border)
                            .w_h(160.0, 30.0)
                            .hover_image(self.imgs.wpn_icon_border_mo)
                            .press_image(self.imgs.wpn_icon_border_press)
                            .down_from(state.ids.weapon_imgs[skill_trees_len - 1], 15.0)
                            .label(&switch_label)
                            .label_font_size(self.fonts.cyri.scale(12))
                            .label_font_id(self.fonts.cyri.conrod_id)
                            .label_color(TEXT_COLOR)
                            .set(state.ids.class_toggle_btn, ui)
                            .was_clicked()
                        {
                            events.push(Event::ChangeSkillTree(SkillGroupKind::Class(other_class)));
                        }

                        let future_levels_checked = character_class.future_levels_to_secondary;
                        if Button::image(if !future_levels_checked {
                            self.imgs.checkbox
                        } else {
                            self.imgs.checkbox_checked
                        })
                        .w_h(18.0, 18.0)
                        .hover_image(if !future_levels_checked {
                            self.imgs.checkbox_mo
                        } else {
                            self.imgs.checkbox_checked_mo
                        })
                        .press_image(if !future_levels_checked {
                            self.imgs.checkbox_press
                        } else {
                            self.imgs.checkbox_checked
                        })
                        .down_from(state.ids.class_toggle_btn, 10.0)
                        .set(state.ids.future_levels_checkbox, ui)
                        .was_clicked()
                        {
                            events.push(Event::SetFutureLevelsToSecondary(!future_levels_checked));
                        }

                        let future_levels_label = self.localized_strings.get_msg_ctx(
                            "hud-skill_tree-multiclass_future_levels",
                            &i18n::fluent_args! {
                                "class" => other_class_name,
                            },
                        );
                        Text::new(&future_levels_label)
                            .right_from(state.ids.future_levels_checkbox, 10.0)
                            .font_size(self.fonts.cyri.scale(12))
                            .font_id(self.fonts.cyri.conrod_id)
                            .graphics_for(state.ids.future_levels_checkbox)
                            .color(TEXT_COLOR)
                            .set(state.ids.future_levels_label, ui);
                    }
                }

                // Exp Bars and Rank Display
                let current_exp = self.skill_set.available_experience(*sel_tab) as f64;
                let max_exp = self.skill_set.skill_point_cost(*sel_tab) as f64;
                let exp_percentage = current_exp / max_exp;
                let rank = self.skill_set.earned_sp(*sel_tab);
                let rank_txt = format!("{}", rank);
                let exp_txt = format!("{}/{}", current_exp, max_exp);
                let available_pts = self.skill_set.available_sp(*sel_tab);
                Image::new(self.imgs.diary_exp_bg)
                    .w_h(480.0, 76.0)
                    .mid_bottom_with_margin_on(state.ids.content_align, 10.0)
                    .set(state.ids.exp_bar_bg, ui);
                Rectangle::fill_with([400.0, 40.0], color::TRANSPARENT)
                    .top_left_with_margins_on(state.ids.exp_bar_bg, 32.0, 40.0)
                    .set(state.ids.exp_bar_content_align, ui);
                Image::new(self.imgs.bar_content)
                    .w_h(400.0 * exp_percentage, 40.0)
                    .top_left_with_margins_on(state.ids.exp_bar_content_align, 0.0, 0.0)
                    .color(Some(XP_COLOR))
                    .set(state.ids.exp_bar_content, ui);
                Image::new(self.imgs.diary_exp_frame)
                    .w_h(480.0, 76.0)
                    .color(Some(UI_HIGHLIGHT_0))
                    .middle_of(state.ids.exp_bar_bg)
                    .set(state.ids.exp_bar_frame, ui);
                // Show as Exp bar below skillbar
                let exp_selected =
                    self.global_state.settings.interface.xp_bar_skillgroup == Some(*sel_tab);
                if Button::image(if !exp_selected {
                    self.imgs.checkbox
                } else {
                    self.imgs.checkbox_checked
                })
                .w_h(18.0, 18.0)
                .hover_image(if !exp_selected {
                    self.imgs.checkbox_mo
                } else {
                    self.imgs.checkbox_checked_mo
                })
                .press_image(if !exp_selected {
                    self.imgs.checkbox_press
                } else {
                    self.imgs.checkbox_checked
                })
                .top_right_with_margins_on(state.ids.exp_bar_frame, 50.0, -30.0)
                .set(state.ids.active_bar_checkbox, ui)
                .was_clicked()
                {
                    if self.global_state.settings.interface.xp_bar_skillgroup != Some(*sel_tab) {
                        events.push(Event::SelectExpBar(Some(*sel_tab)));
                    } else {
                        events.push(Event::SelectExpBar(None));
                    }
                }

                Text::new(&self.localized_strings.get_msg("hud-skill-set_as_exp_bar"))
                    .right_from(state.ids.active_bar_checkbox, 10.0)
                    .font_size(self.fonts.cyri.scale(14))
                    .font_id(self.fonts.cyri.conrod_id)
                    .graphics_for(state.ids.active_bar_checkbox)
                    .color(TEXT_COLOR)
                    .set(state.ids.active_bar_checkbox_label, ui);

                // Show EXP bar text on hover
                if ui
                    .widget_input(state.ids.exp_bar_frame)
                    .mouse()
                    .is_some_and(|m| m.is_over())
                {
                    Text::new(&exp_txt)
                        .mid_top_with_margin_on(state.ids.exp_bar_frame, 47.0)
                        .font_id(self.fonts.cyri.conrod_id)
                        .font_size(self.fonts.cyri.scale(14))
                        .color(TEXT_COLOR)
                        .graphics_for(state.ids.exp_bar_frame)
                        .set(state.ids.exp_bar_txt, ui);
                }
                Text::new(&rank_txt)
                    .mid_top_with_margin_on(state.ids.exp_bar_frame, match rank {
                        0..=99 => 5.0,
                        100..=999 => 8.0,
                        _ => 10.0,
                    })
                    .font_id(self.fonts.cyri.conrod_id)
                    .font_size(self.fonts.cyri.scale(match rank {
                        0..=99 => 28,
                        100..=999 => 21,
                        _ => 15,
                    }))
                    .color(TEXT_COLOR)
                    .set(state.ids.exp_bar_rank, ui);

                Text::new(&self.localized_strings.get_msg_ctx(
                    "hud-skill-sp_available",
                    &i18n::fluent_args! {
                        "number" => available_pts,
                    },
                ))
                .mid_top_with_margin_on(state.ids.content_align, 700.0)
                .font_id(self.fonts.cyri.conrod_id)
                .font_size(self.fonts.cyri.scale(28))
                .color(if available_pts > 0 {
                    Color::Rgba(0.92, 0.76, 0.0, frame_ani)
                } else {
                    TEXT_COLOR
                })
                .set(state.ids.available_pts_txt, ui);
                // Skill Trees
                // Alignment Placing
                let x = 200.0;
                let y = 100.0;
                // Alignment rectangles for skills
                Rectangle::fill_with([124.0 * 2.0, 124.0 * 2.0], color::TRANSPARENT)
                    .top_left_with_margins_on(state.ids.content_align, y, x)
                    .set(state.ids.skills_top_l_align, ui);
                Rectangle::fill_with([124.0 * 2.0, 124.0 * 2.0], color::TRANSPARENT)
                    .top_right_with_margins_on(state.ids.content_align, y, x)
                    .set(state.ids.skills_top_r_align, ui);
                Rectangle::fill_with([124.0 * 2.0, 124.0 * 2.0], color::TRANSPARENT)
                    .bottom_left_with_margins_on(state.ids.content_align, y, x)
                    .set(state.ids.skills_bot_l_align, ui);
                Rectangle::fill_with([124.0 * 2.0, 124.0 * 2.0], color::TRANSPARENT)
                    .bottom_right_with_margins_on(state.ids.content_align, y, x)
                    .set(state.ids.skills_bot_r_align, ui);

                match sel_tab {
                    SelectedSkillTree::General => {
                        self.handle_general_skills_window(&diary_tooltip, state, ui, events)
                    },
                    SelectedSkillTree::Weapon(ToolKind::Sword) => {
                        self.handle_sword_skills_window(&diary_tooltip, state, ui, events)
                    },
                    SelectedSkillTree::Weapon(ToolKind::Axe) => {
                        self.handle_axe_skills_window(&diary_tooltip, state, ui, events)
                    },
                    SelectedSkillTree::Weapon(ToolKind::Hammer) => {
                        self.handle_hammer_skills_window(&diary_tooltip, state, ui, events)
                    },
                    SelectedSkillTree::Weapon(ToolKind::Bow) => {
                        self.handle_bow_skills_window(&diary_tooltip, state, ui, events)
                    },
                    SelectedSkillTree::Weapon(ToolKind::Staff) => {
                        self.handle_staff_skills_window(&diary_tooltip, state, ui, events)
                    },
                    SelectedSkillTree::WeaponRoled(ToolKind::Staff, WeaponRole::Martial) => {
                        self.handle_staff_martial_skills_window(&diary_tooltip, state, ui, events)
                    },
                    SelectedSkillTree::Weapon(ToolKind::Sceptre) => {
                        self.handle_sceptre_skills_window(&diary_tooltip, state, ui, events)
                    },
                    SelectedSkillTree::Weapon(ToolKind::Pick) => {
                        self.handle_mining_skills_window(&diary_tooltip, state, ui, events)
                    },
                    SelectedSkillTree::Class(class) => {
                        self.handle_class_skills_window(*class, &diary_tooltip, state, ui, events)
                    },
                    _ => events,
                }
            },
            DiarySection::AbilitySelection => {
                use comp::ability::AbilityInput;

                // Background Art
                Image::new(self.imgs.book_bg)
                    .w_h(299.0 * 4.0, 184.0 * 4.0)
                    .mid_top_with_margin_on(state.ids.content_align, 4.0)
                    //.graphics_for(state.ids.content_align)
                    .set(state.ids.spellbook_art, ui);
                Image::new(self.imgs.skills_bg)
                    .w_h(240.0 * 2.0, 40.0 * 2.0)
                    .mid_bottom_with_margin_on(state.ids.content_align, 8.0)
                    .set(state.ids.spellbook_skills_bg, ui);

                Rectangle::fill_with([299.0 * 2.0, 184.0 * 4.0], color::TRANSPARENT)
                    .top_left_with_margins_on(state.ids.spellbook_art, 0.0, 0.0)
                    .set(state.ids.sb_page_left_align, ui);
                Rectangle::fill_with([299.0 * 2.0, 184.0 * 4.0], color::TRANSPARENT)
                    .top_right_with_margins_on(state.ids.spellbook_art, 0.0, 0.0)
                    .set(state.ids.sb_page_right_align, ui);

                // Display all active abilities on bottom of window
                state.update(|s| {
                    s.ids
                        .active_abilities
                        .resize(BASE_ABILITY_LIMIT, &mut ui.widget_id_generator())
                });
                state.update(|s| {
                    s.ids
                        .active_abilities_keys
                        .resize(BASE_ABILITY_LIMIT, &mut ui.widget_id_generator())
                });

                let mut slot_maker = SlotMaker {
                    empty_slot: self.imgs.inv_slot,
                    hovered_slot: self.imgs.skillbar_index,
                    filled_slot: self.imgs.inv_slot,
                    selected_slot: self.imgs.inv_slot_sel,
                    background_color: Some(UI_MAIN),
                    content_size: ContentSize {
                        width_height_ratio: 1.0,
                        max_fraction: 0.9,
                    },
                    selected_content_scale: 1.067,
                    amount_font: self.fonts.cyri.conrod_id,
                    amount_margins: Vec2::new(-4.0, 0.0),
                    amount_font_size: self.fonts.cyri.scale(12),
                    amount_text_color: TEXT_COLOR,
                    content_source: &(
                        self.active_abilities,
                        self.ability_pool,
                        self.inventory,
                        self.skill_set,
                        self.stance,
                        self.combo,
                        Some(self.char_state),
                        self.stats,
                        self.buffs,
                    ),
                    image_source: self.imgs,
                    slot_manager: Some(self.slot_manager),
                    global_state: self.global_state,
                    pulse: 0.0,
                };

                for i in 0..BASE_ABILITY_LIMIT {
                    let ability_id = self
                        .active_abilities
                        .get_ability(
                            AbilityInput::Auxiliary(i),
                            Some(self.inventory),
                            Some(self.skill_set),
                            self.stats,
                        )
                        .ability_id(
                            Some(self.char_state),
                            Some(self.inventory),
                            Some(self.skill_set),
                            self.ability_pool,
                            self.stance,
                            self.combo,
                            self.buffs,
                        );
                    let (ability_title, ability_desc) = if let Some(ability_id) = ability_id {
                        util::ability_description(ability_id, self.localized_strings)
                    } else {
                        (
                            Cow::Borrowed("Drag an ability here to use it."),
                            Cow::Borrowed(""),
                        )
                    };

                    let image_size = 80.0;
                    let image_offsets = 92.0 * i as f64;

                    let slot = AbilitySlot::Slot(i);
                    let row_menu_hover = state.active_content == 1 && state.active_row_index == i;
                    let mut ability_slot = slot_maker.fabricate(
                        slot,
                        [image_size; 2],
                        row_menu_hover,
                        ability_row_apply && row_menu_hover,
                    );

                    if i == 0 {
                        ability_slot = ability_slot.top_left_with_margins_on(
                            state.ids.spellbook_skills_bg,
                            0.0,
                            32.0 + image_offsets,
                        );
                    } else {
                        ability_slot =
                            ability_slot.right_from(state.ids.active_abilities[i - 1], 4.0)
                    }
                    ability_slot
                        .with_tooltip(
                            self.tooltip_manager,
                            &ability_title,
                            &ability_desc,
                            &diary_tooltip,
                            TEXT_COLOR,
                        )
                        .set(state.ids.active_abilities[i], ui);

                    // Display Slot Keybinding
                    let keys = &self.global_state.settings.controls;
                    let ability_key = [
                        GameInput::Slot1,
                        GameInput::Slot2,
                        GameInput::Slot3,
                        GameInput::Slot4,
                        GameInput::Slot5,
                    ]
                    .get(i)
                    .and_then(|input| keys.get_binding(*input))
                    .map(|key| key.display_shortest())
                    .unwrap_or_default();

                    Text::new(&ability_key)
                        .top_left_with_margins_on(state.ids.active_abilities[i], 0.0, 4.0)
                        .font_id(self.fonts.cyri.conrod_id)
                        .font_size(self.fonts.cyri.scale(20))
                        .color(TEXT_COLOR)
                        .graphics_for(state.ids.active_abilities[i])
                        .set(state.ids.active_abilities_keys[i], ui);
                }

                let abilities: Vec<_> = ActiveAbilities::all_available_abilities(
                    Some(self.inventory),
                    Some(self.skill_set),
                    self.ability_pool,
                )
                .into_iter()
                .map(|a| {
                    (
                        Ability::from(a).ability_id(
                            Some(self.char_state),
                            Some(self.inventory),
                            Some(self.skill_set),
                            self.ability_pool,
                            self.stance,
                            self.combo,
                            self.buffs,
                        ),
                        a,
                    )
                })
                .collect();

                let page_indices = (abilities.len().saturating_sub(1)) / ABILITIES_PER_PAGE;

                if state.ability_page > page_indices {
                    state.update(|s| s.ability_page = 0);
                }

                state.update(|s| {
                    s.ids
                        .abilities
                        .resize(ABILITIES_PER_PAGE, &mut ui.widget_id_generator())
                });
                state.update(|s| {
                    s.ids
                        .abilities_dual
                        .resize(ABILITIES_PER_PAGE, &mut ui.widget_id_generator())
                });
                state.update(|s| {
                    s.ids
                        .ability_titles
                        .resize(ABILITIES_PER_PAGE, &mut ui.widget_id_generator())
                });
                state.update(|s| {
                    s.ids
                        .ability_frames
                        .resize(ABILITIES_PER_PAGE, &mut ui.widget_id_generator())
                });
                state.update(|s| {
                    s.ids
                        .ability_descs
                        .resize(ABILITIES_PER_PAGE, &mut ui.widget_id_generator())
                });

                // Page button
                // Left Arrow
                let left_arrow = Button::image(if state.ability_page > 0 {
                    self.imgs.arrow_l
                } else {
                    self.imgs.arrow_l_inactive
                })
                .bottom_left_with_margins_on(state.ids.spellbook_art, -83.0, 10.0)
                .w_h(48.0, 55.0);
                // Grey out arrows when inactive
                if state.ability_page > 0 {
                    if left_arrow
                        .hover_image(self.imgs.arrow_l_click)
                        .press_image(self.imgs.arrow_l)
                        .set(state.ids.ability_page_left, ui)
                        .was_clicked()
                    {
                        state.update(|s| s.ability_page -= 1);
                    }
                } else {
                    left_arrow.set(state.ids.ability_page_left, ui);
                }
                // Right Arrow
                let right_arrow = Button::image(if state.ability_page < page_indices {
                    self.imgs.arrow_r
                } else {
                    self.imgs.arrow_r_inactive
                })
                .bottom_right_with_margins_on(state.ids.spellbook_art, -83.0, 10.0)
                .w_h(48.0, 55.0);
                if state.ability_page < page_indices {
                    // Only show right button if not on last page
                    if right_arrow
                        .hover_image(self.imgs.arrow_r_click)
                        .press_image(self.imgs.arrow_r)
                        .set(state.ids.ability_page_right, ui)
                        .was_clicked()
                    {
                        state.update(|s| s.ability_page += 1);
                    };
                } else {
                    right_arrow.set(state.ids.ability_page_right, ui);
                }

                let ability_start = state.ability_page * ABILITIES_PER_PAGE;

                let mut slot_maker = SlotMaker {
                    empty_slot: self.imgs.inv_slot,
                    hovered_slot: self.imgs.skillbar_index,
                    filled_slot: self.imgs.inv_slot,
                    selected_slot: self.imgs.inv_slot_sel,
                    background_color: Some(UI_MAIN),
                    content_size: ContentSize {
                        width_height_ratio: 1.0,
                        max_fraction: 1.0,
                    },
                    selected_content_scale: 1.067,
                    amount_font: self.fonts.cyri.conrod_id,
                    amount_margins: Vec2::new(-4.0, 0.0),
                    amount_font_size: self.fonts.cyri.scale(12),
                    amount_text_color: TEXT_COLOR,
                    content_source: &(
                        self.active_abilities,
                        self.ability_pool,
                        self.inventory,
                        self.skill_set,
                        self.stance,
                        self.combo,
                        Some(self.char_state),
                        self.stats,
                        self.buffs,
                    ),
                    image_source: self.imgs,
                    slot_manager: Some(self.slot_manager),
                    global_state: self.global_state,
                    pulse: 0.0,
                };

                let same_weap_kinds = self
                    .inventory
                    .equipped(EquipSlot::ActiveMainhand)
                    .zip(self.inventory.equipped(EquipSlot::ActiveOffhand))
                    .is_some_and(|(a, b)| {
                        if let (ItemKind::Tool(tool_a), ItemKind::Tool(tool_b)) =
                            (&*a.kind(), &*b.kind())
                        {
                            (a.ability_spec(), tool_a.kind) == (b.ability_spec(), tool_b.kind)
                        } else {
                            false
                        }
                    });

                for (id_index, (ability_id, ability)) in abilities
                    .iter()
                    .skip(ability_start)
                    .take(ABILITIES_PER_PAGE)
                    .enumerate()
                {
                    let (ability_title, ability_desc) =
                        util::ability_description(ability_id.unwrap_or(""), self.localized_strings);

                    let (align_state, image_offsets) = if id_index < ABILITY_GRID_ROWS_PER_COL {
                        (state.ids.sb_page_left_align, 120.0 * id_index as f64)
                    } else {
                        (
                            state.ids.sb_page_right_align,
                            120.0 * (id_index - ABILITY_GRID_ROWS_PER_COL) as f64,
                        )
                    };

                    Image::new(if same_weap_kinds {
                        self.imgs.ability_frame_dual
                    } else {
                        self.imgs.ability_frame
                    })
                    .w_h(566.0, 108.0)
                    .top_left_with_margins_on(align_state, 16.0 + image_offsets, 16.0)
                    .color(Some(UI_HIGHLIGHT_0))
                    .set(state.ids.ability_frames[id_index], ui);

                    let slot = AbilitySlot::Ability(*ability);
                    let grid_menu_hover =
                        state.active_content == 2 && state.active_grid_index == id_index;
                    slot_maker
                        .fabricate(
                            slot,
                            [100.0; 2],
                            grid_menu_hover,
                            ability_grid_apply && grid_menu_hover,
                        )
                        .top_left_with_margins_on(align_state, 20.0 + image_offsets, 20.0)
                        .set(state.ids.abilities[id_index], ui);

                    if same_weap_kinds && let AuxiliaryAbility::MainWeapon(slot) = ability {
                        let ability = AuxiliaryAbility::OffWeapon(*slot);

                        let slot = AbilitySlot::Ability(ability);
                        slot_maker
                            .fabricate(slot, [100.0; 2], false, false)
                            .top_right_with_margins_on(align_state, 20.0 + image_offsets, 20.0)
                            .set(state.ids.abilities_dual[id_index], ui);
                    }
                    // The page width...
                    let text_width = 299.0 * 2.0
                        - if same_weap_kinds && matches!(ability, AuxiliaryAbility::MainWeapon(_)) {
                            // with double the width of an ability image and some padding subtracted
                            // if dual wielding two of the same weapon kind
                            (20.0 + 100.0 + 10.0) * 2.0
                        } else {
                            // or the width of an ability image and some padding subtracted
                            // otherwise
                            20.0 * 2.0 + 100.0
                        };
                    Text::new(&ability_title)
                        .top_left_with_margins_on(state.ids.abilities[id_index], 5.0, 110.0)
                        .font_id(self.fonts.cyri.conrod_id)
                        .font_size(self.fonts.cyri.scale(28))
                        .color(TEXT_COLOR)
                        .w(text_width)
                        .graphics_for(state.ids.abilities[id_index])
                        .set(state.ids.ability_titles[id_index], ui);
                    Text::new(&ability_desc)
                        .top_left_with_margins_on(state.ids.abilities[id_index], 40.0, 110.0)
                        .font_id(self.fonts.cyri.conrod_id)
                        .font_size(self.fonts.cyri.scale(13))
                        .color(TEXT_COLOR)
                        .w(text_width)
                        .graphics_for(state.ids.abilities[id_index])
                        .set(state.ids.ability_descs[id_index], ui);
                }

                events
            },
            // Xindeler: the spell list. Structurally a sibling of the ability
            // tab above — same book background, same five action-bar drop
            // targets at the bottom, same paging — but its rows are driven by
            // the spell compendium rather than by equipment, locked rows are
            // shown greyed with their class+level requirement instead of being
            // hidden, and there is no dual-wield twin slot (that is a
            // weapon-only concept).
            DiarySection::Spells => {
                use common::assets::AssetExt;
                use comp::{
                    ability::{AbilityInput, MagicSource},
                    spell::SpellCompendium,
                };

                /// Tint applied to a locked spell's empty slot.
                const LOCKED_SLOT_COLOR: Color = Color::Rgba(0.35, 0.35, 0.35, 1.0);
                /// Vertical space reserved at the top of each page for the
                /// mastery header (labels at y=14, bars at y=34..48, leaving a
                /// margin before the first spell row). The row loop below
                /// folds this into `image_offsets` so it is the single place
                /// that shifts every row's margins.
                const MASTERY_HEADER_HEIGHT: f64 = 74.0;

                // Background Art
                Image::new(self.imgs.book_bg)
                    .w_h(299.0 * 4.0, 184.0 * 4.0)
                    .mid_top_with_margin_on(state.ids.content_align, 4.0)
                    .set(state.ids.spells_art, ui);
                Image::new(self.imgs.skills_bg)
                    .w_h(240.0 * 2.0, 40.0 * 2.0)
                    .mid_bottom_with_margin_on(state.ids.content_align, 8.0)
                    .set(state.ids.spells_skills_bg, ui);

                Rectangle::fill_with([299.0 * 2.0, 184.0 * 4.0], color::TRANSPARENT)
                    .top_left_with_margins_on(state.ids.spells_art, 0.0, 0.0)
                    .set(state.ids.sp_page_left_align, ui);
                Rectangle::fill_with([299.0 * 2.0, 184.0 * 4.0], color::TRANSPARENT)
                    .top_right_with_margins_on(state.ids.spells_art, 0.0, 0.0)
                    .set(state.ids.sp_page_right_align, ui);

                // Display all active abilities on bottom of window: a spell is
                // dropped onto exactly the same action bar as any other
                // ability.
                state.update(|s| {
                    s.ids
                        .spell_active_abilities
                        .resize(BASE_ABILITY_LIMIT, &mut ui.widget_id_generator())
                });
                state.update(|s| {
                    s.ids
                        .spell_active_abilities_keys
                        .resize(BASE_ABILITY_LIMIT, &mut ui.widget_id_generator())
                });

                let mut slot_maker = SlotMaker {
                    empty_slot: self.imgs.inv_slot,
                    hovered_slot: self.imgs.skillbar_index,
                    filled_slot: self.imgs.inv_slot,
                    selected_slot: self.imgs.inv_slot_sel,
                    background_color: Some(UI_MAIN),
                    content_size: ContentSize {
                        width_height_ratio: 1.0,
                        max_fraction: 0.9,
                    },
                    selected_content_scale: 1.067,
                    amount_font: self.fonts.cyri.conrod_id,
                    amount_margins: Vec2::new(-4.0, 0.0),
                    amount_font_size: self.fonts.cyri.scale(12),
                    amount_text_color: TEXT_COLOR,
                    content_source: &(
                        self.active_abilities,
                        self.ability_pool,
                        self.inventory,
                        self.skill_set,
                        self.stance,
                        self.combo,
                        Some(self.char_state),
                        self.stats,
                        self.buffs,
                    ),
                    image_source: self.imgs,
                    slot_manager: Some(self.slot_manager),
                    global_state: self.global_state,
                    pulse: 0.0,
                };

                for i in 0..BASE_ABILITY_LIMIT {
                    let ability_id = self
                        .active_abilities
                        .get_ability(
                            AbilityInput::Auxiliary(i),
                            Some(self.inventory),
                            Some(self.skill_set),
                            self.stats,
                        )
                        .ability_id(
                            Some(self.char_state),
                            Some(self.inventory),
                            Some(self.skill_set),
                            self.ability_pool,
                            self.stance,
                            self.combo,
                            self.buffs,
                        );
                    let (ability_title, ability_desc) = if let Some(ability_id) = ability_id {
                        util::ability_description(ability_id, self.localized_strings)
                    } else {
                        (
                            Cow::Borrowed("Drag an ability here to use it."),
                            Cow::Borrowed(""),
                        )
                    };

                    let image_size = 80.0;
                    let image_offsets = 92.0 * i as f64;

                    let slot = AbilitySlot::Slot(i);
                    let row_menu_hover = state.active_content == 1 && state.active_row_index == i;
                    let mut ability_slot = slot_maker.fabricate(
                        slot,
                        [image_size; 2],
                        row_menu_hover,
                        ability_row_apply && row_menu_hover,
                    );

                    if i == 0 {
                        ability_slot = ability_slot.top_left_with_margins_on(
                            state.ids.spells_skills_bg,
                            0.0,
                            32.0 + image_offsets,
                        );
                    } else {
                        ability_slot =
                            ability_slot.right_from(state.ids.spell_active_abilities[i - 1], 4.0)
                    }
                    ability_slot
                        .with_tooltip(
                            self.tooltip_manager,
                            &ability_title,
                            &ability_desc,
                            &diary_tooltip,
                            TEXT_COLOR,
                        )
                        .set(state.ids.spell_active_abilities[i], ui);

                    // Display Slot Keybinding
                    let keys = &self.global_state.settings.controls;
                    let ability_key = [
                        GameInput::Slot1,
                        GameInput::Slot2,
                        GameInput::Slot3,
                        GameInput::Slot4,
                        GameInput::Slot5,
                    ]
                    .get(i)
                    .and_then(|input| keys.get_binding(*input))
                    .map(|key| key.display_shortest())
                    .unwrap_or_default();

                    Text::new(&ability_key)
                        .top_left_with_margins_on(state.ids.spell_active_abilities[i], 0.0, 4.0)
                        .font_id(self.fonts.cyri.conrod_id)
                        .font_size(self.fonts.cyri.scale(20))
                        .color(TEXT_COLOR)
                        .graphics_for(state.ids.spell_active_abilities[i])
                        .set(state.ids.spell_active_abilities_keys[i], ui);
                }

                // Every spell of every held class, locked ones included, in
                // pool order — which is already ascending (level, id) per
                // class, i.e. the right reading order, so it is NOT re-sorted
                // here.
                let spells = ActiveAbilities::all_available_spells(
                    self.ability_pool,
                    self.character_class,
                    self.skill_set.character_level(),
                );

                if spells.is_empty() {
                    // Warriors, rogues and the like genuinely have no spells;
                    // say so rather than showing an empty spread.
                    Text::new(&self.localized_strings.get_msg("hud-diary-spells-empty"))
                        .mid_top_with_margin_on(state.ids.spells_art, 320.0)
                        .font_id(self.fonts.cyri.conrod_id)
                        .font_size(self.fonts.cyri.scale(28))
                        .color(TEXT_COLOR)
                        .set(state.ids.spell_empty_txt, ui);

                    return events;
                }

                let compendium = SpellCompendium::load_expect("common.spells.compendium");
                let compendium = compendium.read();

                // Per-source mastery header: Arcane is known by default and
                // shown as a plain label; the other four sources each get a
                // progress bar sourced from `SpellMastery::pct`. Positioned
                // relative to `spells_art` directly (not a page-align
                // rectangle) so it renders identically regardless of which
                // page of the spell list is showing.
                {
                    const MASTERY_SOURCES: [MagicSource; 4] = [
                        MagicSource::Divine,
                        MagicSource::Primordial,
                        MagicSource::Psionic,
                        MagicSource::Ki,
                    ];
                    /// Width of one header column (label + bar).
                    const COL_W: f64 = 224.0;
                    /// Gap between header columns.
                    const COL_GAP: f64 = 12.0;
                    const BAR_W: f64 = 200.0;
                    const BAR_H: f64 = 14.0;
                    const LABEL_Y: f64 = 14.0;
                    const BAR_Y: f64 = 34.0;
                    const LEFT_MARGIN: f64 = 20.0;

                    state.update(|s| {
                        let id_gen = &mut ui.widget_id_generator();
                        s.ids.spell_mastery_labels.resize(5, id_gen);
                        s.ids
                            .spell_mastery_bar_bg
                            .resize(MASTERY_SOURCES.len(), id_gen);
                        s.ids
                            .spell_mastery_bar_content
                            .resize(MASTERY_SOURCES.len(), id_gen);
                    });

                    let default_mastery = comp::SpellMastery::default();
                    let mastery = self.spell_mastery.unwrap_or(&default_mastery);

                    // Arcane column: always "known", never a bar (mastery
                    // never applies to it — `SpellMastery::pct` hardcodes
                    // 1.0 for it too).
                    let arcane_label = format!(
                        "{} — {}",
                        magic_source_name(MagicSource::Arcane, self.localized_strings),
                        self.localized_strings
                            .get_msg("hud-diary-spells-mastery-arcane-known"),
                    );
                    Text::new(&arcane_label)
                        .top_left_with_margins_on(state.ids.spells_art, LABEL_Y, LEFT_MARGIN)
                        .font_id(self.fonts.cyri.conrod_id)
                        .font_size(self.fonts.cyri.scale(14))
                        .color(TEXT_COLOR)
                        .set(state.ids.spell_mastery_labels[0], ui);

                    for (i, source) in MASTERY_SOURCES.into_iter().enumerate() {
                        let pct = mastery.pct(source);
                        // Whether the compendium carries any spell of this
                        // source at all — distinct from whether the player
                        // has UNLOCKED any of them. A source with none must
                        // read as "nothing here yet", not a bare, unexplained
                        // 0 % bar.
                        let has_content = compendium.iter().any(|def| def.source == source);
                        let x = LEFT_MARGIN + (i + 1) as f64 * (COL_W + COL_GAP);

                        let label = if has_content {
                            self.localized_strings.get_msg_ctx(
                                "hud-diary-spells-mastery-pct",
                                &i18n::fluent_args! {
                                    "source" => magic_source_name(source, self.localized_strings).into_owned(),
                                    "pct" => (pct * 100.0).round() as u32,
                                },
                            )
                        } else {
                            self.localized_strings.get_msg_ctx(
                                "hud-diary-spells-mastery-empty-source",
                                &i18n::fluent_args! {
                                    "source" => magic_source_name(source, self.localized_strings).into_owned(),
                                },
                            )
                        };
                        Text::new(&label)
                            .top_left_with_margins_on(state.ids.spells_art, LABEL_Y, x)
                            .font_id(self.fonts.cyri.conrod_id)
                            .font_size(self.fonts.cyri.scale(14))
                            .color(TEXT_COLOR)
                            .set(state.ids.spell_mastery_labels[i + 1], ui);

                        let tooltip_title = magic_source_name(source, self.localized_strings);
                        let tooltip_body =
                            mastery_tooltip_body(has_content, self.localized_strings);

                        Rectangle::fill_with([BAR_W, BAR_H], Color::Rgba(0.0, 0.0, 0.0, 0.35))
                            .top_left_with_margins_on(state.ids.spells_art, BAR_Y, x)
                            .with_tooltip(
                                self.tooltip_manager,
                                &tooltip_title,
                                &tooltip_body,
                                &diary_tooltip,
                                TEXT_COLOR,
                            )
                            .set(state.ids.spell_mastery_bar_bg[i], ui);

                        Image::new(self.imgs.bar_content)
                            .w_h((BAR_W * pct as f64).max(0.0), BAR_H)
                            .top_left_with_margins_on(state.ids.spell_mastery_bar_bg[i], 0.0, 0.0)
                            .color(Some(mastery_bar_color(source)))
                            .graphics_for(state.ids.spell_mastery_bar_bg[i])
                            .set(state.ids.spell_mastery_bar_content[i], ui);
                    }
                }

                let page_indices = (spells.len().saturating_sub(1)) / SPELLS_PER_PAGE;

                // Multiclassing mid-session changes the spell count, which can
                // strand the view past the last page.
                if state.spell_page > page_indices {
                    state.update(|s| s.spell_page = 0);
                }

                state.update(|s| {
                    let id_gen = &mut ui.widget_id_generator();
                    s.ids.spell_slots.resize(SPELLS_PER_PAGE, id_gen);
                    s.ids.spell_locked_slots.resize(SPELLS_PER_PAGE, id_gen);
                    s.ids.spell_locks.resize(SPELLS_PER_PAGE, id_gen);
                    s.ids.spell_frames.resize(SPELLS_PER_PAGE, id_gen);
                    s.ids.spell_titles.resize(SPELLS_PER_PAGE, id_gen);
                    s.ids.spell_metas.resize(SPELLS_PER_PAGE, id_gen);
                    s.ids.spell_reqs.resize(SPELLS_PER_PAGE, id_gen);
                });

                // Page buttons
                // Left Arrow
                let left_arrow = Button::image(if state.spell_page > 0 {
                    self.imgs.arrow_l
                } else {
                    self.imgs.arrow_l_inactive
                })
                .bottom_left_with_margins_on(state.ids.spells_art, -83.0, 10.0)
                .w_h(48.0, 55.0);
                // Grey out arrows when inactive
                if state.spell_page > 0 {
                    if left_arrow
                        .hover_image(self.imgs.arrow_l_click)
                        .press_image(self.imgs.arrow_l)
                        .set(state.ids.spell_page_left, ui)
                        .was_clicked()
                    {
                        state.update(|s| s.spell_page -= 1);
                    }
                } else {
                    left_arrow.set(state.ids.spell_page_left, ui);
                }
                // Right Arrow
                let right_arrow = Button::image(if state.spell_page < page_indices {
                    self.imgs.arrow_r
                } else {
                    self.imgs.arrow_r_inactive
                })
                .bottom_right_with_margins_on(state.ids.spells_art, -83.0, 10.0)
                .w_h(48.0, 55.0);
                if state.spell_page < page_indices {
                    // Only show right button if not on last page
                    if right_arrow
                        .hover_image(self.imgs.arrow_r_click)
                        .press_image(self.imgs.arrow_r)
                        .set(state.ids.spell_page_right, ui)
                        .was_clicked()
                    {
                        state.update(|s| s.spell_page += 1);
                    };
                } else {
                    right_arrow.set(state.ids.spell_page_right, ui);
                }

                let spell_start = state.spell_page * SPELLS_PER_PAGE;

                let mut slot_maker = SlotMaker {
                    empty_slot: self.imgs.inv_slot,
                    hovered_slot: self.imgs.skillbar_index,
                    filled_slot: self.imgs.inv_slot,
                    selected_slot: self.imgs.inv_slot_sel,
                    background_color: Some(UI_MAIN),
                    content_size: ContentSize {
                        width_height_ratio: 1.0,
                        max_fraction: 1.0,
                    },
                    selected_content_scale: 1.067,
                    amount_font: self.fonts.cyri.conrod_id,
                    amount_margins: Vec2::new(-4.0, 0.0),
                    amount_font_size: self.fonts.cyri.scale(12),
                    amount_text_color: TEXT_COLOR,
                    content_source: &(
                        self.active_abilities,
                        self.ability_pool,
                        self.inventory,
                        self.skill_set,
                        self.stance,
                        self.combo,
                        Some(self.char_state),
                        self.stats,
                        self.buffs,
                    ),
                    image_source: self.imgs,
                    slot_manager: Some(self.slot_manager),
                    global_state: self.global_state,
                    pulse: 0.0,
                };

                // A row's text column: the page width less the slot and its
                // padding.
                let text_width = 299.0 * 2.0 - (20.0 * 2.0 + 100.0);

                for (id_index, (ability, unlocked)) in spells
                    .iter()
                    .skip(spell_start)
                    .take(SPELLS_PER_PAGE)
                    .enumerate()
                {
                    // Every entry `all_available_spells` yields is an `Innate`
                    // pool index by construction; the pool key at that index is
                    // the compendium id.
                    let pool_index = match ability {
                        AuxiliaryAbility::Innate(i) => Some(*i),
                        _ => None,
                    };
                    let pool = self.ability_pool;
                    let pool_key = pool_index
                        .and_then(|i| pool.and_then(|p| p.abilities.get(i)))
                        .map(String::as_str);
                    let spell = pool_key.and_then(|key| compendium.get(key));

                    let (align_state, image_offsets) = if id_index < SPELL_GRID_ROWS_PER_COL {
                        (
                            state.ids.sp_page_left_align,
                            MASTERY_HEADER_HEIGHT + 120.0 * id_index as f64,
                        )
                    } else {
                        (
                            state.ids.sp_page_right_align,
                            MASTERY_HEADER_HEIGHT
                                + 120.0 * (id_index - SPELL_GRID_ROWS_PER_COL) as f64,
                        )
                    };

                    Image::new(self.imgs.ability_frame)
                        .w_h(566.0, 108.0)
                        .top_left_with_margins_on(align_state, 16.0 + image_offsets, 16.0)
                        .color(Some(UI_HIGHLIGHT_0))
                        .set(state.ids.spell_frames[id_index], ui);

                    // A gated pool key without a compendium entry cannot happen
                    // today (the gate is derived from the entry), but show the
                    // raw key rather than a blank row if content ever drifts.
                    let title = match spell {
                        Some(spell) => self.localized_strings.get_msg(&spell.name_i18n),
                        None => Cow::Borrowed(pool_key.unwrap_or_default()),
                    };
                    // Most spells already have authored prose; a few do not, and
                    // for those the tooltip is simply empty rather than showing a
                    // raw i18n key.
                    let description = spell
                        .and_then(|spell| self.localized_strings.try_msg(&spell.description_i18n))
                        .unwrap_or(Cow::Borrowed(""));

                    // Locked spells are shown, but cannot be picked up: an
                    // un-draggable tinted slot with a padlock stands in for the
                    // real ability slot.
                    let anchor_id = if *unlocked {
                        let slot = AbilitySlot::Ability(*ability);
                        let grid_menu_hover =
                            state.active_content == 2 && state.active_grid_index == id_index;
                        slot_maker
                            .fabricate(
                                slot,
                                [100.0; 2],
                                grid_menu_hover,
                                ability_grid_apply && grid_menu_hover,
                            )
                            .top_left_with_margins_on(align_state, 20.0 + image_offsets, 20.0)
                            .with_tooltip(
                                self.tooltip_manager,
                                &title,
                                &description,
                                &diary_tooltip,
                                TEXT_COLOR,
                            )
                            .set(state.ids.spell_slots[id_index], ui);
                        state.ids.spell_slots[id_index]
                    } else {
                        Image::new(self.imgs.inv_slot)
                            .w_h(100.0, 100.0)
                            .top_left_with_margins_on(align_state, 20.0 + image_offsets, 20.0)
                            .color(Some(LOCKED_SLOT_COLOR))
                            .with_tooltip(
                                self.tooltip_manager,
                                &title,
                                &description,
                                &diary_tooltip,
                                TEXT_COLOR,
                            )
                            .set(state.ids.spell_locked_slots[id_index], ui);
                        Image::new(self.imgs.lock)
                            .w_h(50.0, 50.0)
                            .middle_of(state.ids.spell_locked_slots[id_index])
                            .graphics_for(state.ids.spell_locked_slots[id_index])
                            .set(state.ids.spell_locks[id_index], ui);
                        state.ids.spell_locked_slots[id_index]
                    };
                    Text::new(&title)
                        .top_left_with_margins_on(align_state, 25.0 + image_offsets, 130.0)
                        .font_id(self.fonts.cyri.conrod_id)
                        .font_size(self.fonts.cyri.scale(28))
                        .color(TEXT_COLOR)
                        .w(text_width)
                        .graphics_for(anchor_id)
                        .set(state.ids.spell_titles[id_index], ui);

                    // The metadata line is what this tab carries instead of
                    // prose: it is derived wholly from the compendium entry, so
                    // it needs no per-spell text to be authored.
                    if let Some(spell) = spell {
                        let meta = spell_meta_line(spell, self.localized_strings);
                        Text::new(&meta)
                            .top_left_with_margins_on(align_state, 62.0 + image_offsets, 130.0)
                            .font_id(self.fonts.cyri.conrod_id)
                            .font_size(self.fonts.cyri.scale(14))
                            .color(TEXT_COLOR)
                            .w(text_width)
                            .graphics_for(anchor_id)
                            .set(state.ids.spell_metas[id_index], ui);
                    }

                    if !*unlocked
                        && let Some(gate) =
                            pool_index.and_then(|i| pool.and_then(|p| p.spell_gate(i)))
                    {
                        let requirement = spell_requirement(
                            gate,
                            self.character_class,
                            self.skill_set.character_level(),
                            self.localized_strings,
                        );
                        Text::new(&requirement)
                            .top_left_with_margins_on(align_state, 84.0 + image_offsets, 130.0)
                            .font_id(self.fonts.cyri.conrod_id)
                            .font_size(self.fonts.cyri.scale(14))
                            .color(CRITICAL_HP_COLOR)
                            .w(text_width)
                            .graphics_for(anchor_id)
                            .set(state.ids.spell_reqs[id_index], ui);
                    }
                }

                events
            },
            DiarySection::Character => {
                // Background Art
                Image::new(self.imgs.book_bg)
                    .w_h(299.0 * 4.0, 184.0 * 4.0)
                    .mid_top_with_margin_on(state.ids.content_align, 4.0)
                    .set(state.ids.spellbook_art, ui);

                if state.ids.stat_names.len() < STAT_COUNT {
                    state.update(|s| {
                        s.ids
                            .stat_names
                            .resize(STAT_COUNT, &mut ui.widget_id_generator());
                        s.ids
                            .stat_values
                            .resize(STAT_COUNT, &mut ui.widget_id_generator());
                    });
                }

                // The viewer's own cached gear aggregates, fetched once for the
                // whole sheet. Attunement gating (ENG-D2c) is already folded
                // into `protection`.
                let derived_stats = self.client.state().ecs().read_storage::<DerivedStats>();
                let derived = derived_stats.get(self.client.entity());

                for (i, stat) in CharacterStat::iter().enumerate() {
                    // Stat names
                    let localized_name = stat.localized_str(self.localized_strings);
                    let mut txt = Text::new(&localized_name)
                        .font_id(self.fonts.cyri.conrod_id)
                        .font_size(self.fonts.cyri.scale(29))
                        .color(BLACK);

                    if i == 0 {
                        txt = txt.top_left_with_margins_on(state.ids.spellbook_art, 20.0, 20.0);
                    } else {
                        txt = txt.down_from(state.ids.stat_names[i - 1], 10.0);
                    };
                    txt.set(state.ids.stat_names[i], ui);

                    let main_weap_stats = self
                        .inventory
                        .equipped(EquipSlot::ActiveMainhand)
                        .and_then(|item| match &*item.kind() {
                            ItemKind::Tool(tool) => {
                                Some(tool.stats(item.stats_durability_multiplier()))
                            },
                            _ => None,
                        });

                    let off_weap_stats = self
                        .inventory
                        .equipped(EquipSlot::ActiveOffhand)
                        .and_then(|item| match &*item.kind() {
                            ItemKind::Tool(tool) => {
                                Some(tool.stats(item.stats_durability_multiplier()))
                            },
                            _ => None,
                        });

                    let (name, _gender, battle_mode) = self
                        .client
                        .player_list()
                        .get(self.uid)
                        .and_then(|info| info.character.as_ref())
                        .map_or_else(
                            || ("Unknown".to_string(), None, BattleMode::PvP),
                            |character_info| {
                                (
                                    self.localized_strings.get_content(&character_info.name),
                                    character_info.gender,
                                    character_info.battle_mode,
                                )
                            },
                        );

                    // Stat values
                    let value = match stat {
                        CharacterStat::Name => name,
                        CharacterStat::Level => {
                            let character_level = self.skill_set.character_level();
                            match self.character_class.filter(|cc| cc.is_multiclass()) {
                                Some(character_class) => {
                                    let class_name = |class: ClassKind| {
                                        let key =
                                            format!("char_selection-class_{}", class.keyword());
                                        self.localized_strings.get_msg(&key).into_owned()
                                    };
                                    let levels = character_class
                                        .class_levels(character_level)
                                        .map(|(class, level, _)| {
                                            format!("{} {level}", class_name(class))
                                        })
                                        .collect::<Vec<_>>()
                                        .join(" / ");
                                    format!("{character_level} ({levels})")
                                },
                                None => format!("{character_level}"),
                            }
                        },
                        CharacterStat::BattleMode => match battle_mode {
                            BattleMode::PvP => "PvP".to_string(),
                            BattleMode::PvE => "PvE".to_string(),
                        },
                        CharacterStat::Waypoint => self
                            .client
                            .waypoint()
                            .map(|c| self.localized_strings.get_content(c))
                            .unwrap_or_else(|| {
                                self.localized_strings
                                    .get_msg("char_selection-uncanny_valley")
                                    .into_owned()
                            }),
                        CharacterStat::Hitpoints => format!("{}", self.health.maximum() as u32),
                        CharacterStat::Energy => format!("{}", self.energy.maximum() as u32),
                        CharacterStat::Poise => format!("{}", self.poise.maximum() as u32),
                        CharacterStat::CombatRating => {
                            let cr = derived.map_or(0.0, |d| d.combat_rating);
                            format!("{:.2}", cr * 10.0)
                        },
                        CharacterStat::Protection => {
                            match derived.map_or(Some(0.0), |d| d.protection) {
                                Some(prot) => format!("{}", prot),
                                None => String::from("Invincible"),
                            }
                        },
                        CharacterStat::StunResistance => {
                            let stun_res =
                                Poise::compute_poise_damage_reduction(derived, None, self.stats);
                            format!("{:.2}%", stun_res * 100.0)
                        },
                        CharacterStat::PrecisionPower => {
                            let precision_power = derived
                                .map_or(DerivedStats::DEFAULT_PRECISION_MULT, |d| d.precision_mult);
                            format!("x{:.2}", precision_power)
                        },
                        CharacterStat::EnergyReward => {
                            let energy_rew = derived.map_or(1.0, |d| d.energy_reward_mod);
                            format!("{:+.0}%", (energy_rew - 1.0) * 100.0)
                        },
                        CharacterStat::Stealth => {
                            // The player's own readout of their own concealment,
                            // not an observer looking at them, so there is no
                            // observer-side pierce-concealment to apply.
                            let stealth_perception_multiplier =
                                combat::perception_dist_multiplier_from_stealth(
                                    derived, None, self.stats, None,
                                );
                            let txt =
                                format!("{:+.1}%", (1.0 - stealth_perception_multiplier) * 100.0);

                            txt
                        },
                        CharacterStat::WeaponPower => match (main_weap_stats, off_weap_stats) {
                            (Some(m_stats), Some(o_stats)) => {
                                format!("{}   {}", m_stats.power * 10.0, o_stats.power * 10.0)
                            },
                            (Some(stats), None) | (None, Some(stats)) => {
                                format!("{}", stats.power * 10.0)
                            },
                            (None, None) => String::new(),
                        },
                        CharacterStat::WeaponSpeed => {
                            let spd_fmt = |sp| (sp - 1.0) * 100.0;
                            match (main_weap_stats, off_weap_stats) {
                                (Some(m_stats), Some(o_stats)) => format!(
                                    "{:+.0}%   {:+.0}%",
                                    spd_fmt(m_stats.speed),
                                    spd_fmt(o_stats.speed)
                                ),
                                (Some(stats), None) | (None, Some(stats)) => {
                                    format!("{:+.0}%", spd_fmt(stats.speed))
                                },
                                _ => String::new(),
                            }
                        },
                        CharacterStat::WeaponEffectPower => match (main_weap_stats, off_weap_stats)
                        {
                            (Some(m_stats), Some(o_stats)) => {
                                format!(
                                    "{}   {}",
                                    m_stats.effect_power * 10.0,
                                    o_stats.effect_power * 10.0
                                )
                            },
                            (Some(stats), None) | (None, Some(stats)) => {
                                format!("{}", stats.effect_power * 10.0)
                            },
                            (None, None) => String::new(),
                        },
                        CharacterStat::Pact => match self
                            .character_class
                            .filter(|cc| cc.classes().any(|c| c == ClassKind::Warlock))
                        {
                            Some(_) => {
                                let standing = match self.pact.map(|p| p.standing) {
                                    Some(PactStanding::Severed) => {
                                        self.localized_strings.get_msg("hud-warlock-pact-severed")
                                    },
                                    Some(PactStanding::Bound) | None => {
                                        self.localized_strings.get_msg("hud-warlock-pact-bound")
                                    },
                                };
                                let patron = self
                                    .pact
                                    .and_then(|p| p.patron)
                                    .map(|patron| {
                                        self.localized_strings
                                            .get_msg(&patron.name_i18n_key())
                                            .into_owned()
                                    })
                                    .unwrap_or_else(|| {
                                        self.localized_strings
                                            .get_msg("hud-warlock-pact-no_patron")
                                            .into_owned()
                                    });
                                format!("{standing} — {patron}")
                            },
                            None => String::new(),
                        },
                    };

                    let mut number = Text::new(&value)
                        .font_id(self.fonts.cyri.conrod_id)
                        .font_size(self.fonts.cyri.scale(29))
                        .color(BLACK);

                    if i == 0 {
                        number = number.right_from(state.ids.stat_names[i], 165.0);
                    } else {
                        number = number.down_from(state.ids.stat_values[i - 1], 10.0);
                    };
                    number.set(state.ids.stat_values[i], ui);
                }

                events
            },
            DiarySection::Recipes => {
                // Background Art
                Image::new(self.imgs.book_bg)
                    .w_h(299.0 * 4.0, 184.0 * 4.0)
                    .mid_top_with_margin_on(state.ids.content_align, 4.0)
                    .set(state.ids.spellbook_art, ui);

                Rectangle::fill_with([299.0 * 2.0, 184.0 * 4.0], color::TRANSPARENT)
                    .top_left_with_margins_on(state.ids.spellbook_art, 0.0, 0.0)
                    .set(state.ids.sb_page_left_align, ui);
                Rectangle::fill_with([299.0 * 2.0, 184.0 * 4.0], color::TRANSPARENT)
                    .top_right_with_margins_on(state.ids.spellbook_art, 0.0, 0.0)
                    .set(state.ids.sb_page_right_align, ui);

                const RECIPES_PER_PAGE: usize = 36;

                let page_index_max =
                    self.inventory.recipe_groups_iter().len().saturating_sub(1) / RECIPES_PER_PAGE;

                if state.recipe_page > page_index_max {
                    state.update(|s| s.recipe_page = 0);
                }

                // Page button
                // Left Arrow
                let left_arrow = Button::image(if state.recipe_page > 0 {
                    self.imgs.arrow_l
                } else {
                    self.imgs.arrow_l_inactive
                })
                .bottom_left_with_margins_on(state.ids.spellbook_art, -83.0, 10.0)
                .w_h(48.0, 55.0);
                // Grey out arrows when inactive
                if state.recipe_page > 0 {
                    if left_arrow
                        .hover_image(self.imgs.arrow_l_click)
                        .press_image(self.imgs.arrow_l)
                        .set(state.ids.ability_page_left, ui)
                        .was_clicked()
                    {
                        state.update(|s| s.recipe_page -= 1);
                    }
                } else {
                    left_arrow.set(state.ids.ability_page_left, ui);
                }
                // Right Arrow
                let right_arrow = Button::image(if state.recipe_page < page_index_max {
                    self.imgs.arrow_r
                } else {
                    self.imgs.arrow_r_inactive
                })
                .bottom_right_with_margins_on(state.ids.spellbook_art, -83.0, 10.0)
                .w_h(48.0, 55.0);
                if state.recipe_page < page_index_max {
                    // Only show right button if not on last page
                    if right_arrow
                        .hover_image(self.imgs.arrow_r_click)
                        .press_image(self.imgs.arrow_r)
                        .set(state.ids.ability_page_right, ui)
                        .was_clicked()
                    {
                        state.update(|s| s.recipe_page += 1);
                    };
                } else {
                    right_arrow.set(state.ids.ability_page_right, ui);
                }

                state.update(|s| {
                    s.ids
                        .recipe_groups
                        .resize(RECIPES_PER_PAGE, &mut ui.widget_id_generator())
                });

                for (i, rg) in self
                    .inventory
                    .recipe_groups_iter()
                    .skip(state.recipe_page * RECIPES_PER_PAGE)
                    .take(RECIPES_PER_PAGE)
                    .enumerate()
                {
                    let (title, _desc) =
                        util::item_text(rg, self.localized_strings, self.item_i18n);

                    let mut text = Text::new(&title)
                        .font_id(self.fonts.cyri.conrod_id)
                        .font_size(self.fonts.cyri.scale(29))
                        .color(BLACK);

                    if i == 0 {
                        text =
                            text.top_left_with_margins_on(state.ids.sb_page_left_align, 20.0, 20.0);
                    } else if i == 18 {
                        text = text.top_left_with_margins_on(
                            state.ids.sb_page_right_align,
                            20.0,
                            20.0,
                        );
                    } else {
                        text = text.down_from(state.ids.recipe_groups[i - 1], 10.0);
                    }
                    text.set(state.ids.recipe_groups[i], ui);
                }

                events
            },
        }
    }
}

/// Xindeler: the red "Requires &lt;class&gt; level N" line under a spell the
/// character cannot cast yet.
///
/// THE ONE PLACE that turns a spell gate into a requirement string. A spell
/// can be granted by more than one held class, so this names the grantor that
/// unlocks it SOONEST for this character (the one already at the highest class
/// level) rather than an arbitrary one — a Mage 1 / Cleric 40 reads "Requires
/// Cleric level 42", never "Requires Mage level 42". Both the grantor choice
/// and the required level come from the gate itself, so this only formats.
fn spell_requirement(
    gate: &SpellGate,
    character_class: Option<&CharacterClass>,
    character_level: u16,
    i18n: &Localization,
) -> Cow<'static, str> {
    // Falls back to the first recorded grantor when the character holds none
    // of them, so the line still names a class instead of rendering blank.
    let class_name = gate
        .nearest_grantor(character_class, character_level)
        .map(|(class, _)| class)
        .or_else(|| gate.classes().next())
        .map(|class| {
            i18n.get_msg(&format!("char_selection-class_{}", class.keyword()))
                .into_owned()
        })
        .unwrap_or_default();
    i18n.get_msg_ctx("hud-diary-spells-locked", &i18n::fluent_args! {
        "class" => class_name,
        "level" => gate.required_class_level(),
    })
}

/// Xindeler: the one-line summary shown under a spell's name in the Diary
/// spell tab, e.g. `Level 3 · Evocation · Arcane · Action · 30 m · Sphere 6 m ·
/// Instant`.
///
/// Every part comes from the spell compendium entry, so the tab stays
/// informative without any per-spell prose having been authored; the only
/// strings it needs are the ~30 shared enum names in `hud/spells.ftl`.
/// Optional dimensions (school, area of effect) are simply omitted when the
/// spell has none.
fn spell_meta_line(spell: &comp::spell::SpellDef, i18n: &Localization) -> String {
    use comp::{
        ability::{MagicSource, School},
        spell::{CastTime, SpellAoe, SpellDuration, SpellRange},
    };

    // Distances and durations are authored as floats but are whole numbers in
    // practice; render `30 m`, not `30.0 m`.
    fn num(v: f32) -> String {
        if v.fract() == 0.0 {
            format!("{}", v as i64)
        } else {
            format!("{v}")
        }
    }

    let mut parts: Vec<Cow<str>> = Vec::with_capacity(7);

    parts.push(if spell.level == 0 {
        i18n.get_msg("hud-diary-spells-cantrip")
    } else {
        i18n.get_msg_ctx("hud-diary-spells-level", &i18n::fluent_args! {
            "level" => spell.level,
        })
    });

    if let Some(school) = spell.school {
        parts.push(i18n.get_msg(match school {
            School::Abjuration => "hud-diary-spells-school-abjuration",
            School::Conjuration => "hud-diary-spells-school-conjuration",
            School::Divination => "hud-diary-spells-school-divination",
            School::Enchantment => "hud-diary-spells-school-enchantment",
            School::Evocation => "hud-diary-spells-school-evocation",
            School::Illusion => "hud-diary-spells-school-illusion",
            School::Necromancy => "hud-diary-spells-school-necromancy",
            School::Transmutation => "hud-diary-spells-school-transmutation",
            School::Axiomancy => "hud-diary-spells-school-axiomancy",
            School::Hemomancy => "hud-diary-spells-school-hemomancy",
        }));
    }

    parts.push(i18n.get_msg(match spell.source {
        MagicSource::Arcane => "hud-diary-spells-source-arcane",
        MagicSource::Divine => "hud-diary-spells-source-divine",
        MagicSource::Primordial => "hud-diary-spells-source-primordial",
        MagicSource::Psionic => "hud-diary-spells-source-psionic",
        MagicSource::Ki => "hud-diary-spells-source-ki",
    }));

    parts.push(match spell.cast_time {
        CastTime::Action => i18n.get_msg("hud-diary-spells-cast-action"),
        CastTime::Bonus => i18n.get_msg("hud-diary-spells-cast-bonus"),
        CastTime::Reaction => i18n.get_msg("hud-diary-spells-cast-reaction"),
        CastTime::Minutes(minutes) => {
            i18n.get_msg_ctx("hud-diary-spells-cast-minutes", &i18n::fluent_args! {
                "minutes" => minutes,
            })
        },
    });

    parts.push(match spell.range {
        SpellRange::SelfOnly => i18n.get_msg("hud-diary-spells-range-self"),
        SpellRange::Touch => i18n.get_msg("hud-diary-spells-range-touch"),
        SpellRange::Meters(m) => {
            i18n.get_msg_ctx("hud-diary-spells-range-meters", &i18n::fluent_args! {
                "meters" => num(m),
            })
        },
    });

    if let Some(aoe) = spell.aoe {
        let (key, size) = match aoe {
            SpellAoe::Sphere(size) => ("hud-diary-spells-aoe-sphere", size),
            SpellAoe::Cone(size) => ("hud-diary-spells-aoe-cone", size),
            SpellAoe::Line(size) => ("hud-diary-spells-aoe-line", size),
            SpellAoe::Cube(size) => ("hud-diary-spells-aoe-cube", size),
        };
        parts.push(i18n.get_msg_ctx(key, &i18n::fluent_args! { "size" => num(size) }));
    }

    parts.push(match spell.duration {
        SpellDuration::Instant => i18n.get_msg("hud-diary-spells-duration-instant"),
        SpellDuration::Secs(secs) => {
            i18n.get_msg_ctx("hud-diary-spells-duration-secs", &i18n::fluent_args! {
                "secs" => num(secs),
            })
        },
        SpellDuration::Concentration(secs) => i18n.get_msg_ctx(
            "hud-diary-spells-duration-concentration",
            &i18n::fluent_args! { "secs" => num(secs) },
        ),
    });

    parts.join(" · ")
}

/// The localized display name of a magic source, shared by the spell
/// metadata line and the mastery header.
fn magic_source_name(source: comp::ability::MagicSource, i18n: &Localization) -> Cow<'_, str> {
    use comp::ability::MagicSource;

    i18n.get_msg(match source {
        MagicSource::Arcane => "hud-diary-spells-source-arcane",
        MagicSource::Divine => "hud-diary-spells-source-divine",
        MagicSource::Primordial => "hud-diary-spells-source-primordial",
        MagicSource::Psionic => "hud-diary-spells-source-psionic",
        MagicSource::Ki => "hud-diary-spells-source-ki",
    })
}

/// A stable, source-distinguishing tint for the mastery bars. Purely
/// cosmetic — carries no gameplay meaning beyond "which source is this".
/// `Arcane` is included for exhaustiveness but never actually drawn as a
/// bar (it has no mastery bar at all).
fn mastery_bar_color(source: comp::ability::MagicSource) -> Color {
    use comp::ability::MagicSource;

    match source {
        MagicSource::Arcane => XP_COLOR,
        MagicSource::Divine => Color::Rgba(0.85, 0.75, 0.35, 1.0),
        MagicSource::Primordial => Color::Rgba(0.40, 0.68, 0.35, 1.0),
        MagicSource::Psionic => Color::Rgba(0.58, 0.38, 0.78, 1.0),
        MagicSource::Ki => Color::Rgba(0.80, 0.35, 0.25, 1.0),
    }
}

/// The mastery bar's hover tooltip: what each tier of a source's mastery
/// percentage unlocks for spell transcription (the copy-into-`SpellBook`
/// mechanic; `common::comp::spell_mastery::mastery_tier_max_level` is the
/// authoritative curve this describes). Deliberately a DIFFERENT message
/// from `spell_requirement`'s red "Requires <class> level N" line under a
/// locked spell row: that one explains a `SpellGate` cast-eligibility check,
/// this one explains a mastery-tier copy-eligibility check. The two checks
/// are independent (a spell needs both to ever be cast), so conflating
/// their messages would read as one gate rather than two.
fn mastery_tooltip_body(has_content: bool, i18n: &Localization) -> String {
    let mut lines = vec![
        i18n.get_msg("hud-diary-spells-mastery-tooltip-tier-2")
            .into_owned(),
        i18n.get_msg("hud-diary-spells-mastery-tooltip-tier-4")
            .into_owned(),
        i18n.get_msg("hud-diary-spells-mastery-tooltip-tier-6")
            .into_owned(),
        i18n.get_msg("hud-diary-spells-mastery-tooltip-tier-all")
            .into_owned(),
    ];
    if !has_content {
        lines.push(
            i18n.get_msg("hud-diary-spells-mastery-tooltip-no-content")
                .into_owned(),
        );
    }
    lines.join("\n")
}

enum SkillIcon<'a> {
    Unlockable {
        skill: Skill,
        image: image::Id,
        position: PositionSpecifier,
        id: widget::Id,
    },
    Descriptive {
        title: &'a str,
        desc: &'a str,
        image: image::Id,
        position: PositionSpecifier,
        id: widget::Id,
    },
    Ability {
        skill: Skill,
        ability_id: &'a str,
        position: PositionSpecifier,
    },
}

impl Diary<'_> {
    // --- BL-06 P3a helpers -------------------------------------------------

    /// Returns the class skill group currently shown in the (single) Class
    /// tab: whichever of the held classes matches `sel_tab` (the in-tab
    /// toggle picks between them by pushing `Event::ChangeSkillTree`, same
    /// as every other tab), defaulting to the primary otherwise — including
    /// on first open, before either has been explicitly selected. `None` for
    /// Adventurer / unclassed characters.
    fn selected_class_group(&self) -> Option<SkillGroupKind> {
        let character_class = self.character_class?;
        if character_class.primary == ClassKind::Adventurer {
            return None;
        }
        let sel_tab = self.show.diary_fields.skilltreetab;
        let secondary_group = character_class.secondary.map(SkillGroupKind::Class);
        let group = if secondary_group == Some(sel_tab) {
            sel_tab
        } else {
            SkillGroupKind::Class(character_class.primary)
        };
        self.skill_set
            .skill_groups()
            .any(|sg| sg.skill_group_kind == group)
            .then_some(group)
    }

    /// Compute the tier (depth) of `skill` in the prerequisite DAG.
    /// Tier 0 = no prerequisites (root node).
    /// Tier N = 1 + max tier of any direct prerequisite.
    fn class_skill_tier(skill: Skill, depth: u8) -> u8 {
        if depth > 8 {
            // Guard against unexpected cycles / pathological graphs.
            return 0;
        }
        match SKILL_PREREQUISITES.get(&skill) {
            None => 0,
            Some(SkillPrerequisite::All(map) | SkillPrerequisite::Any(map)) => {
                let max_prereq_tier = map
                    .keys()
                    .map(|p| Self::class_skill_tier(*p, depth + 1))
                    .max()
                    .unwrap_or(0);
                max_prereq_tier + 1
            },
        }
    }

    /// Maps the 8 active class skills to their `ability_id` strings (the keys
    /// used in the ability manifests). Returns None for passive skills.
    fn class_skill_ability_id(skill: Skill) -> Option<&'static str> {
        match skill {
            Skill::Warrior(WarriorSkill::Rally) => Some("class.warrior.rally"),
            Skill::Warrior(WarriorSkill::Onslaught) => Some("class.warrior.onslaught"),
            Skill::Mage(MageSkill::ArcaneSurge) => Some("class.mage.arcanesurge"),
            Skill::Mage(MageSkill::ArcaneMastery) => Some("class.mage.arcanemastery"),
            Skill::Cleric(ClericSkill::MendingLight) => Some("class.cleric.mendinglight"),
            Skill::Cleric(ClericSkill::RadiantChannel) => Some("class.cleric.radiantchannel"),
            Skill::Rogue(RogueSkill::Ambush) => Some("class.rogue.ambush"),
            Skill::Rogue(RogueSkill::Vanish) => Some("class.rogue.vanish"),
            _ => None,
        }
    }

    /// Map a `ClassPassiveStat` to a representative existing icon.
    fn class_passive_icon(&self, stat: Option<&(skills::ClassPassiveStat, f32)>) -> image::Id {
        use skills::ClassPassiveStat::*;
        match stat.map(|(s, _)| *s) {
            Some(MaxHealth) => self.imgs.buff_healthplus_0,
            Some(MaxEnergy) => self.imgs.buff_energyplus_0,
            Some(AttackDamage) => self.imgs.buff_damage_skill,
            Some(SpellPower) => self.imgs.magic_damage_skill,
            Some(HealPower) => self.imgs.magic_lifesteal_skill,
            Some(Accuracy | MagicAccuracy) => self.imgs.magic_distance_skill,
            Some(CritChance | PrecisionMult) => self.imgs.buff_imminentcritical,
            Some(Evasion) => self.imgs.buff_haste_0,
            Some(DamageReduction | MitigationsPenetration) => self.imgs.buff_dmg_red_0,
            Some(ResistFire) => self.imgs.buff_flame,
            Some(ResistFrost) => self.imgs.buff_frigid,
            Some(ResistPoison) => self.imgs.magic_cost_skill,
            Some(ResistMagic) => self.imgs.magic_radius_skill,
            Some(CrowdControlResistance) => self.imgs.buff_fortitude_0,
            Some(PoiseDamage) => self.imgs.buff_frenzy_0,
            Some(MoveSpeed | RecoverySpeed) => self.imgs.utility_speed_skill,
            Some(EnergyReward) => self.imgs.magic_energy_regen_skill,
            Some(EnergyEfficiency) => self.imgs.magic_cost_skill,
            Some(EnergyRegen) => self.imgs.magic_energy_regen_skill,
            Some(BonusVs(_)) => self.imgs.buff_plus_0,
            None => self.imgs.buff_cost_skill,
        }
    }

    /// Generic class skill-tree renderer — serves all 14 classes from the RON
    /// manifests without hand-laying a grid. Skills are sorted into tier rows
    /// (tier 0 at the top) and positioned inside `state.ids.content_align`.
    ///
    /// Layout constants (tunable for visual polish in-game):
    ///   TOP_MARGIN  = 80 px  (below the tree title)
    ///   ROW_H       = 120 px (vertical gap between tiers)
    ///   COL_W       = 110 px (horizontal gap between skills in a row)
    ///   ICON_SIZE   = 74 px  (matches existing Unlockable buttons)
    fn handle_class_skills_window(
        &mut self,
        class: ClassKind,
        diary_tooltip: &Tooltip,
        state: &mut State<DiaryState>,
        ui: &mut UiCell,
        mut events: Vec<Event>,
    ) -> Vec<Event> {
        use PositionSpecifier::TopLeftWithMarginsOn;

        // Title — generic localized header (the active class is conveyed by the
        // selected tab + the character sheet, so this avoids needing a localized
        // name key for every one of the 14 classes).
        let title = self.localized_strings.get_msg("hud-skill_tree-class-title");
        Text::new(&title)
            .mid_top_with_margin_on(state.ids.content_align, 2.0)
            .font_id(self.fonts.cyri.conrod_id)
            .font_size(self.fonts.cyri.scale(34))
            .color(TEXT_COLOR)
            .set(state.ids.tree_title_txt, ui);

        // Background alignment rectangle for the skill grid
        const GRID_W: f64 = 900.0;
        const GRID_H: f64 = 600.0;
        Rectangle::fill_with([GRID_W, GRID_H], color::TRANSPARENT)
            .mid_top_with_margin_on(state.ids.content_align, 50.0)
            .set(state.ids.class_tree_align, ui);

        // Collect the skills for this class from the manifest (BTreeSet → Vec
        // gives a stable, deterministic order across runs).
        let skills: Vec<Skill> = SKILL_GROUP_DEFS
            .get(&SkillGroupKind::Class(class))
            .map(|def| def.skills.iter().copied().collect())
            .unwrap_or_default();

        if skills.is_empty() {
            // No skills yet for this class (10 classes are empty stubs). Use a
            // dedicated widget id so it doesn't clobber the title above.
            let empty = self.localized_strings.get_msg("hud-skill_tree-class-empty");
            Text::new(&empty)
                .mid_top_with_margin_on(state.ids.class_tree_align, 40.0)
                .font_id(self.fonts.cyri.conrod_id)
                .font_size(self.fonts.cyri.scale(20))
                .color(TEXT_COLOR)
                .set(state.ids.class_tree_empty_txt, ui);
            return events;
        }

        // Compute tier for each skill.
        let tiers: Vec<u8> = skills
            .iter()
            .map(|&s| Self::class_skill_tier(s, 0))
            .collect();

        let max_tier = tiers.iter().copied().max().unwrap_or(0);

        // Group skills by tier to compute row widths for centering.
        let mut rows: Vec<Vec<usize>> = vec![vec![]; (max_tier as usize) + 1];
        for (idx, &tier) in tiers.iter().enumerate() {
            rows[tier as usize].push(idx);
        }

        // Layout constants
        const TOP_MARGIN: f64 = 30.0;
        const ROW_H: f64 = 120.0;
        const COL_W: f64 = 110.0;

        // Build the SkillIcon list in (tier, column) order.
        let mut skill_icons: Vec<SkillIcon> = Vec::with_capacity(skills.len());

        for (tier_idx, row_indices) in rows.iter().enumerate() {
            let row_count = row_indices.len();
            // Centre the row within the grid width.
            let total_row_w = (row_count as f64 - 1.0) * COL_W;
            let row_x_start = (GRID_W - total_row_w) / 2.0;
            let y = TOP_MARGIN + tier_idx as f64 * ROW_H;

            for (col, &skill_idx) in row_indices.iter().enumerate() {
                let skill = skills[skill_idx];
                let x = row_x_start + col as f64 * COL_W;
                let position = TopLeftWithMarginsOn(state.ids.class_tree_align, y, x);

                if let Some(ability_id) = Self::class_skill_ability_id(skill) {
                    // Active ability skill
                    skill_icons.push(SkillIcon::Ability {
                        skill,
                        ability_id,
                        position,
                    });
                } else {
                    // Passive skill — pick an icon based on what stat it boosts
                    let first_stat = CLASS_SKILL_MODIFIERS.get(&skill).and_then(|v| v.first());
                    let image = self.class_passive_icon(first_stat);
                    skill_icons.push(SkillIcon::Unlockable {
                        skill,
                        image,
                        position,
                        // id is filled in below once we have the id vec
                        id: state.ids.class_tree_align, // placeholder, overwritten below
                    });
                }
            }
        }

        // Allocate widget-id arrays to match the skill count.
        // `class_skills` backs Unlockable buttons (via pre-set `.id` field).
        // `skills` / `skill_lock_imgs` back Ability buttons (used by
        // `create_unlock_ability_button` via the slice index `i`).
        let n = skill_icons.len();
        state.update(|s| s.ids.class_skills.resize(n, &mut ui.widget_id_generator()));
        state.update(|s| {
            s.ids
                .class_skill_lock_imgs
                .resize(n, &mut ui.widget_id_generator())
        });
        state.update(|s| s.ids.skills.resize(n, &mut ui.widget_id_generator()));
        state.update(|s| {
            s.ids
                .skill_lock_imgs
                .resize(n, &mut ui.widget_id_generator())
        });

        // Resolve widget ids for Unlockable entries (abilities carry no id field).
        let mut unlockable_counter = 0usize;
        let final_icons: Vec<SkillIcon> = skill_icons
            .into_iter()
            .map(|icon| match icon {
                SkillIcon::Unlockable {
                    skill,
                    image,
                    position,
                    ..
                } => {
                    let id = state.ids.class_skills[unlockable_counter];
                    unlockable_counter += 1;
                    SkillIcon::Unlockable {
                        skill,
                        image,
                        position,
                        id,
                    }
                },
                other => other,
            })
            .collect();

        self.handle_skill_buttons(&final_icons, ui, &mut events, diary_tooltip, state);
        events
    }

    // --- end BL-06 P3a helpers ----------------------------------------------

    fn handle_general_skills_window(
        &mut self,
        diary_tooltip: &Tooltip,
        state: &mut State<DiaryState>,
        ui: &mut UiCell,
        mut events: Vec<Event>,
    ) -> Vec<Event> {
        let tree_title = &self.localized_strings.get_msg("common-weapons-general");
        Text::new(tree_title)
            .mid_top_with_margin_on(state.ids.content_align, 2.0)
            .font_id(self.fonts.cyri.conrod_id)
            .font_size(self.fonts.cyri.scale(34))
            .color(TEXT_COLOR)
            .set(state.ids.tree_title_txt, ui);

        // Number of skills per rectangle per weapon, start counting at 0
        // Maximum of 9 skills/8 indices
        let skills_top_l = 7;
        let skills_top_r = 0;
        let skills_bot_l = 0;
        let skills_bot_r = 5;

        self.setup_state_for_skill_icons(
            state,
            ui,
            skills_top_l,
            skills_top_r,
            skills_bot_l,
            skills_bot_r,
        );

        use SkillGroupKind::*;
        use ToolKind::*;
        // General Combat
        Image::new(animate_by_pulse(
            &self.item_imgs.img_ids_or_not_found_img(ItemKey::Simple(
                "example_general_combat_left".to_string(),
            )),
            self.pulse,
        ))
        .wh(ART_SIZE)
        .middle_of(state.ids.content_align)
        .color(Some(Color::Rgba(1.0, 1.0, 1.0, 1.0)))
        .set(state.ids.general_combat_render_0, ui);

        Image::new(animate_by_pulse(
            &self.item_imgs.img_ids_or_not_found_img(ItemKey::Simple(
                "example_general_combat_right".to_string(),
            )),
            self.pulse,
        ))
        .wh(ART_SIZE)
        .middle_of(state.ids.general_combat_render_0)
        .color(Some(Color::Rgba(1.0, 1.0, 1.0, 1.0)))
        .set(state.ids.general_combat_render_1, ui);

        use PositionSpecifier::MidTopWithMarginOn;
        let skill_buttons = &[
            // Top Left skills
            //        5 1 6
            //        3 0 4
            //        8 2 7
            // Bottom left skills
            SkillIcon::Unlockable {
                skill: Skill::UnlockGroup(Weapon(Sword)),
                image: self.imgs.unlock_sword_skill,
                position: MidTopWithMarginOn(state.ids.skills_top_l[0], 3.0),
                id: state.ids.skill_general_tree_0,
            },
            SkillIcon::Unlockable {
                skill: Skill::UnlockGroup(Weapon(Axe)),
                image: self.imgs.unlock_axe_skill,
                position: MidTopWithMarginOn(state.ids.skills_top_l[1], 3.0),
                id: state.ids.skill_general_tree_1,
            },
            SkillIcon::Unlockable {
                skill: Skill::UnlockGroup(Weapon(Hammer)),
                image: self.imgs.unlock_hammer_skill,
                position: MidTopWithMarginOn(state.ids.skills_top_l[2], 3.0),
                id: state.ids.skill_general_tree_2,
            },
            SkillIcon::Unlockable {
                skill: Skill::UnlockGroup(Weapon(Bow)),
                image: self.imgs.unlock_bow_skill,
                position: MidTopWithMarginOn(state.ids.skills_top_l[3], 3.0),
                id: state.ids.skill_general_tree_3,
            },
            SkillIcon::Unlockable {
                skill: Skill::UnlockGroup(Weapon(Staff)),
                image: self.imgs.unlock_staff_skill0,
                position: MidTopWithMarginOn(state.ids.skills_top_l[4], 3.0),
                id: state.ids.skill_general_tree_4,
            },
            SkillIcon::Unlockable {
                skill: Skill::UnlockGroup(Weapon(Sceptre)),
                image: self.imgs.unlock_sceptre_skill,
                position: MidTopWithMarginOn(state.ids.skills_top_l[5], 3.0),
                id: state.ids.skill_general_tree_5,
            },
            SkillIcon::Unlockable {
                skill: Skill::UnlockGroup(WeaponRoled(Staff, WeaponRole::Martial)),
                image: self.imgs.unlock_staff_skill0,
                position: MidTopWithMarginOn(state.ids.skills_top_l[6], 3.0),
                id: state.ids.skill_general_tree_6,
            },
            // Bottom right skills
            SkillIcon::Descriptive {
                title: "hud-skill-climbing_title",
                desc: "hud-skill-climbing",
                image: self.imgs.skill_climbing_skill,
                position: MidTopWithMarginOn(state.ids.skills_bot_r[0], 3.0),
                id: state.ids.skill_general_climb_0,
            },
            SkillIcon::Unlockable {
                skill: Skill::Climb(ClimbSkill::Cost),
                image: self.imgs.utility_cost_skill,
                position: MidTopWithMarginOn(state.ids.skills_bot_r[1], 3.0),
                id: state.ids.skill_general_climb_1,
            },
            SkillIcon::Unlockable {
                skill: Skill::Climb(ClimbSkill::Speed),
                image: self.imgs.utility_speed_skill,
                position: MidTopWithMarginOn(state.ids.skills_bot_r[2], 3.0),
                id: state.ids.skill_general_climb_2,
            },
            SkillIcon::Descriptive {
                title: "hud-skill-swim_title",
                desc: "hud-skill-swim",
                image: self.imgs.skill_swim_skill,
                position: MidTopWithMarginOn(state.ids.skills_bot_r[3], 3.0),
                id: state.ids.skill_general_swim_0,
            },
            SkillIcon::Unlockable {
                skill: Skill::Swim(SwimSkill::Speed),
                image: self.imgs.utility_speed_skill,
                position: MidTopWithMarginOn(state.ids.skills_bot_r[4], 3.0),
                id: state.ids.skill_general_swim_1,
            },
        ];

        self.handle_skill_buttons(skill_buttons, ui, &mut events, diary_tooltip, state);
        events
    }

    fn handle_sword_skills_window(
        &mut self,
        diary_tooltip: &Tooltip,
        state: &mut State<DiaryState>,
        ui: &mut UiCell,
        mut events: Vec<Event>,
    ) -> Vec<Event> {
        Image::new(self.imgs.sword_tree_paths)
            .wh([1042.0, 636.0])
            .mid_top_with_margin_on(state.ids.content_align, 55.0)
            .graphics_for(state.ids.content_align)
            .color(Some(Color::Rgba(1.0, 1.0, 1.0, 1.0)))
            .set(state.ids.sword_path_overlay, ui);

        // Sword
        Image::new(self.imgs.sword_bg)
            .wh([933.0, 615.0])
            .mid_top_with_margin_on(state.ids.content_align, 65.0)
            .color(Some(Color::Rgba(1.0, 1.0, 1.0, 1.0)))
            .set(state.ids.sword_bg, ui);

        // Stances:
        // For alignment purposes
        Rectangle::fill_with([169.0, 615.0], color::TRANSPARENT)
            .top_left_of(state.ids.sword_bg)
            .set(state.ids.sword_stance_left_align, ui);
        Rectangle::fill_with([169.0, 615.0], color::TRANSPARENT)
            .top_right_of(state.ids.sword_bg)
            .set(state.ids.sword_stance_right_align, ui);

        // Cleaving
        Text::new(
            &self
                .localized_strings
                .get_msg("hud-skill-sword_stance_cleaving"),
        )
        .mid_top_with_margin_on(state.ids.sword_stance_left_align, -7.0)
        .font_id(self.fonts.cyri.conrod_id)
        .font_size(self.fonts.cyri.scale(34))
        .color(Color::Rgba(0.94, 0.54, 0.07, 1.0))
        .set(state.ids.sword_stance_cleaving_text, ui);

        Text::new(
            &self
                .localized_strings
                .get_msg("hud-skill-sword_stance_cleaving"),
        )
        .x_y_position_relative_to(
            state.ids.sword_stance_cleaving_text,
            Relative::Scalar(2.5),
            Relative::Scalar(-2.5),
        )
        .depth(1.0)
        .font_id(self.fonts.cyri.conrod_id)
        .font_size(self.fonts.cyri.scale(34))
        .color(color::BLACK)
        .set(state.ids.sword_stance_cleaving_shadow, ui);

        // Agile
        Text::new(
            &self
                .localized_strings
                .get_msg("hud-skill-sword_stance_agile"),
        )
        .mid_top_with_margin_on(state.ids.sword_bg, -7.0)
        .font_id(self.fonts.cyri.conrod_id)
        .font_size(self.fonts.cyri.scale(34))
        .color(Color::Rgba(0.81, 0.70, 0.08, 1.0))
        .set(state.ids.sword_stance_agile_text, ui);

        Text::new(
            &self
                .localized_strings
                .get_msg("hud-skill-sword_stance_agile"),
        )
        .x_y_position_relative_to(
            state.ids.sword_stance_agile_text,
            Relative::Scalar(2.5),
            Relative::Scalar(-2.5),
        )
        .depth(1.0)
        .font_id(self.fonts.cyri.conrod_id)
        .font_size(self.fonts.cyri.scale(34))
        .color(color::BLACK)
        .set(state.ids.sword_stance_agile_shadow, ui);

        // Crippling
        Text::new(
            &self
                .localized_strings
                .get_msg("hud-skill-sword_stance_crippling"),
        )
        .mid_top_with_margin_on(state.ids.sword_stance_right_align, -7.0)
        .font_id(self.fonts.cyri.conrod_id)
        .font_size(self.fonts.cyri.scale(34))
        .color(Color::Rgba(0.0, 0.52, 0.0, 1.0))
        .set(state.ids.sword_stance_crippling_text, ui);

        Text::new(
            &self
                .localized_strings
                .get_msg("hud-skill-sword_stance_crippling"),
        )
        .x_y_position_relative_to(
            state.ids.sword_stance_crippling_text,
            Relative::Scalar(2.5),
            Relative::Scalar(-2.5),
        )
        .depth(1.0)
        .font_id(self.fonts.cyri.conrod_id)
        .font_size(self.fonts.cyri.scale(34))
        .color(color::BLACK)
        .set(state.ids.sword_stance_crippling_shadow, ui);

        // Heavy
        Text::new(
            &self
                .localized_strings
                .get_msg("hud-skill-sword_stance_heavy"),
        )
        .mid_bottom_with_margin_on(state.ids.sword_stance_left_align, 272.0)
        .font_id(self.fonts.cyri.conrod_id)
        .font_size(self.fonts.cyri.scale(34))
        .color(Color::Rgba(0.67, 0.0, 0.0, 1.0))
        .set(state.ids.sword_stance_heavy_text, ui);

        Text::new(
            &self
                .localized_strings
                .get_msg("hud-skill-sword_stance_heavy"),
        )
        .x_y_position_relative_to(
            state.ids.sword_stance_heavy_text,
            Relative::Scalar(2.5),
            Relative::Scalar(-2.5),
        )
        .depth(1.0)
        .font_id(self.fonts.cyri.conrod_id)
        .font_size(self.fonts.cyri.scale(34))
        .color(color::BLACK)
        .set(state.ids.sword_stance_heavy_shadow, ui);

        // Defensive
        Text::new(
            &self
                .localized_strings
                .get_msg("hud-skill-sword_stance_defensive"),
        )
        .mid_bottom_with_margin_on(state.ids.sword_stance_right_align, 272.0)
        .font_id(self.fonts.cyri.conrod_id)
        .font_size(self.fonts.cyri.scale(34))
        .color(Color::Rgba(0.10, 0.40, 0.82, 1.0))
        .set(state.ids.sword_stance_defensive_text, ui);

        Text::new(
            &self
                .localized_strings
                .get_msg("hud-skill-sword_stance_defensive"),
        )
        .x_y_position_relative_to(
            state.ids.sword_stance_defensive_text,
            Relative::Scalar(2.5),
            Relative::Scalar(-2.5),
        )
        .depth(1.0)
        .font_id(self.fonts.cyri.conrod_id)
        .font_size(self.fonts.cyri.scale(34))
        .color(color::BLACK)
        .set(state.ids.sword_stance_defensive_shadow, ui);

        use PositionSpecifier::TopLeftWithMarginsOn;
        let skill_buttons = &[
            SkillIcon::Ability {
                skill: Skill::Sword(SwordSkill::CrescentSlash),
                ability_id: "veloren.core.pseudo_abilities.sword.crescent_slash",
                position: TopLeftWithMarginsOn(state.ids.sword_bg, 537.0, 429.0),
            },
            SkillIcon::Ability {
                skill: Skill::Sword(SwordSkill::FellStrike),
                ability_id: "veloren.core.pseudo_abilities.sword.fell_strike",
                position: TopLeftWithMarginsOn(state.ids.sword_bg, 457.0, 527.0),
            },
            SkillIcon::Ability {
                skill: Skill::Sword(SwordSkill::Skewer),
                ability_id: "veloren.core.pseudo_abilities.sword.skewer",
                position: TopLeftWithMarginsOn(state.ids.sword_bg, 368.0, 527.0),
            },
            SkillIcon::Ability {
                skill: Skill::Sword(SwordSkill::Cascade),
                ability_id: "veloren.core.pseudo_abilities.sword.cascade",
                position: TopLeftWithMarginsOn(state.ids.sword_bg, 457.0, 332.0),
            },
            SkillIcon::Ability {
                skill: Skill::Sword(SwordSkill::CrossCut),
                ability_id: "veloren.core.pseudo_abilities.sword.cross_cut",
                position: TopLeftWithMarginsOn(state.ids.sword_bg, 368.0, 332.0),
            },
            SkillIcon::Ability {
                skill: Skill::Sword(SwordSkill::Finisher),
                ability_id: "veloren.core.pseudo_abilities.sword.finisher",
                position: TopLeftWithMarginsOn(state.ids.sword_bg, 263.0, 429.0),
            },
            SkillIcon::Ability {
                skill: Skill::Sword(SwordSkill::HeavySweep),
                ability_id: "common.abilities.sword.heavy_sweep",
                position: TopLeftWithMarginsOn(state.ids.sword_bg, 457.0, 2.0),
            },
            SkillIcon::Ability {
                skill: Skill::Sword(SwordSkill::HeavyPommelStrike),
                ability_id: "common.abilities.sword.heavy_pommel_strike",
                position: TopLeftWithMarginsOn(state.ids.sword_bg, 457.0, 91.0),
            },
            SkillIcon::Ability {
                skill: Skill::Sword(SwordSkill::AgileQuickDraw),
                ability_id: "common.abilities.sword.agile_quick_draw",
                position: TopLeftWithMarginsOn(state.ids.sword_bg, 142.0, 384.0),
            },
            SkillIcon::Ability {
                skill: Skill::Sword(SwordSkill::AgileFeint),
                ability_id: "common.abilities.sword.agile_feint",
                position: TopLeftWithMarginsOn(state.ids.sword_bg, 142.0, 472.0),
            },
            SkillIcon::Ability {
                skill: Skill::Sword(SwordSkill::DefensiveRiposte),
                ability_id: "common.abilities.sword.defensive_riposte",
                position: TopLeftWithMarginsOn(state.ids.sword_bg, 457.0, 766.0),
            },
            SkillIcon::Ability {
                skill: Skill::Sword(SwordSkill::DefensiveDisengage),
                ability_id: "common.abilities.sword.defensive_disengage",
                position: TopLeftWithMarginsOn(state.ids.sword_bg, 457.0, 855.0),
            },
            SkillIcon::Ability {
                skill: Skill::Sword(SwordSkill::CripplingGouge),
                ability_id: "common.abilities.sword.crippling_gouge",
                position: TopLeftWithMarginsOn(state.ids.sword_bg, 53.0, 766.0),
            },
            SkillIcon::Ability {
                skill: Skill::Sword(SwordSkill::CripplingHamstring),
                ability_id: "common.abilities.sword.crippling_hamstring",
                position: TopLeftWithMarginsOn(state.ids.sword_bg, 142.0, 766.0),
            },
            SkillIcon::Ability {
                skill: Skill::Sword(SwordSkill::CleavingWhirlwindSlice),
                ability_id: "common.abilities.sword.cleaving_whirlwind_slice",
                position: TopLeftWithMarginsOn(state.ids.sword_bg, 53.0, 91.0),
            },
            SkillIcon::Ability {
                skill: Skill::Sword(SwordSkill::CleavingEarthSplitter),
                ability_id: "common.abilities.sword.cleaving_earth_splitter",
                position: TopLeftWithMarginsOn(state.ids.sword_bg, 142.0, 91.0),
            },
            SkillIcon::Ability {
                skill: Skill::Sword(SwordSkill::HeavyFortitude),
                ability_id: "common.abilities.sword.heavy_fortitude",
                position: TopLeftWithMarginsOn(state.ids.sword_bg, 368.0, 2.0),
            },
            SkillIcon::Ability {
                skill: Skill::Sword(SwordSkill::HeavyPillarThrust),
                ability_id: "common.abilities.sword.heavy_pillar_thrust",
                position: TopLeftWithMarginsOn(state.ids.sword_bg, 368.0, 91.0),
            },
            SkillIcon::Ability {
                skill: Skill::Sword(SwordSkill::AgileDancingEdge),
                ability_id: "common.abilities.sword.agile_dancing_edge",
                position: TopLeftWithMarginsOn(state.ids.sword_bg, 53.0, 385.0),
            },
            SkillIcon::Ability {
                skill: Skill::Sword(SwordSkill::AgileFlurry),
                ability_id: "common.abilities.sword.agile_flurry",
                position: TopLeftWithMarginsOn(state.ids.sword_bg, 53.0, 473.0),
            },
            SkillIcon::Ability {
                skill: Skill::Sword(SwordSkill::DefensiveStalwartSword),
                ability_id: "common.abilities.sword.defensive_stalwart_sword",
                position: TopLeftWithMarginsOn(state.ids.sword_bg, 368.0, 766.0),
            },
            SkillIcon::Ability {
                skill: Skill::Sword(SwordSkill::DefensiveDeflect),
                ability_id: "common.abilities.sword.defensive_deflect",
                position: TopLeftWithMarginsOn(state.ids.sword_bg, 368.0, 855.0),
            },
            SkillIcon::Ability {
                skill: Skill::Sword(SwordSkill::CripplingEviscerate),
                ability_id: "common.abilities.sword.crippling_eviscerate",
                position: TopLeftWithMarginsOn(state.ids.sword_bg, 142.0, 855.0),
            },
            SkillIcon::Ability {
                skill: Skill::Sword(SwordSkill::CripplingBloodyGash),
                ability_id: "common.abilities.sword.crippling_bloody_gash",
                position: TopLeftWithMarginsOn(state.ids.sword_bg, 53.0, 855.0),
            },
            SkillIcon::Ability {
                skill: Skill::Sword(SwordSkill::CleavingBladeFever),
                ability_id: "common.abilities.sword.cleaving_blade_fever",
                position: TopLeftWithMarginsOn(state.ids.sword_bg, 53.0, 2.0),
            },
            SkillIcon::Ability {
                skill: Skill::Sword(SwordSkill::CleavingSkySplitter),
                ability_id: "common.abilities.sword.cleaving_sky_splitter",
                position: TopLeftWithMarginsOn(state.ids.sword_bg, 142.0, 2.0),
            },
        ];

        state.update(|s| {
            s.ids
                .skills
                .resize(skill_buttons.len(), &mut ui.widget_id_generator())
        });
        state.update(|s| {
            s.ids
                .skill_lock_imgs
                .resize(skill_buttons.len(), &mut ui.widget_id_generator())
        });

        self.handle_skill_buttons(skill_buttons, ui, &mut events, diary_tooltip, state);
        events
    }

    fn handle_axe_skills_window(
        &mut self,
        diary_tooltip: &Tooltip,
        state: &mut State<DiaryState>,
        ui: &mut UiCell,
        mut events: Vec<Event>,
    ) -> Vec<Event> {
        // Axe
        Image::new(self.imgs.axe_bg)
            .wh([924.0, 619.0])
            .mid_top_with_margin_on(state.ids.content_align, 65.0)
            .color(Some(Color::Rgba(1.0, 1.0, 1.0, 1.0)))
            .set(state.ids.axe_bg, ui);

        use PositionSpecifier::TopLeftWithMarginsOn;
        let skill_buttons = &[
            SkillIcon::Ability {
                skill: Skill::Axe(AxeSkill::BrutalSwing),
                ability_id: "common.abilities.axe.brutal_swing",
                position: TopLeftWithMarginsOn(state.ids.axe_bg, 387.0, 424.0),
            },
            SkillIcon::Ability {
                skill: Skill::Axe(AxeSkill::Berserk),
                ability_id: "common.abilities.axe.berserk",
                position: TopLeftWithMarginsOn(state.ids.axe_bg, 287.0, 374.0),
            },
            SkillIcon::Ability {
                skill: Skill::Axe(AxeSkill::RisingTide),
                ability_id: "common.abilities.axe.rising_tide",
                position: TopLeftWithMarginsOn(state.ids.axe_bg, 287.0, 474.0),
            },
            SkillIcon::Ability {
                skill: Skill::Axe(AxeSkill::SavageSense),
                ability_id: "common.abilities.axe.savage_sense",
                position: TopLeftWithMarginsOn(state.ids.axe_bg, 187.0, 324.0),
            },
            SkillIcon::Ability {
                skill: Skill::Axe(AxeSkill::AdrenalineRush),
                ability_id: "common.abilities.axe.adrenaline_rush",
                position: TopLeftWithMarginsOn(state.ids.axe_bg, 187.0, 524.0),
            },
            SkillIcon::Ability {
                skill: Skill::Axe(AxeSkill::Execute),
                ability_id: "common.abilities.axe.execute",
                position: TopLeftWithMarginsOn(state.ids.axe_bg, 187.0, 424.0),
            },
            SkillIcon::Ability {
                skill: Skill::Axe(AxeSkill::Maelstrom),
                ability_id: "common.abilities.axe.maelstrom",
                position: TopLeftWithMarginsOn(state.ids.axe_bg, 4.0, 424.0),
            },
            SkillIcon::Ability {
                skill: Skill::Axe(AxeSkill::Rake),
                ability_id: "common.abilities.axe.rake",
                position: TopLeftWithMarginsOn(state.ids.axe_bg, 507.0, 325.0),
            },
            SkillIcon::Ability {
                skill: Skill::Axe(AxeSkill::Bloodfeast),
                ability_id: "common.abilities.axe.bloodfeast",
                position: TopLeftWithMarginsOn(state.ids.axe_bg, 387.0, 74.0),
            },
            SkillIcon::Ability {
                skill: Skill::Axe(AxeSkill::FierceRaze),
                ability_id: "common.abilities.axe.fierce_raze",
                position: TopLeftWithMarginsOn(state.ids.axe_bg, 387.0, 174.0),
            },
            SkillIcon::Ability {
                skill: Skill::Axe(AxeSkill::Furor),
                ability_id: "common.abilities.axe.furor",
                position: TopLeftWithMarginsOn(state.ids.axe_bg, 287.0, 24.0),
            },
            SkillIcon::Ability {
                skill: Skill::Axe(AxeSkill::Fracture),
                ability_id: "common.abilities.axe.fracture",
                position: TopLeftWithMarginsOn(state.ids.axe_bg, 287.0, 224.0),
            },
            SkillIcon::Ability {
                skill: Skill::Axe(AxeSkill::Lacerate),
                ability_id: "common.abilities.axe.lacerate",
                position: TopLeftWithMarginsOn(state.ids.axe_bg, 287.0, 124.0),
            },
            SkillIcon::Ability {
                skill: Skill::Axe(AxeSkill::Riptide),
                ability_id: "common.abilities.axe.riptide",
                position: TopLeftWithMarginsOn(state.ids.axe_bg, 104.0, 124.0),
            },
            SkillIcon::Ability {
                skill: Skill::Axe(AxeSkill::SkullBash),
                ability_id: "common.abilities.axe.skull_bash",
                position: TopLeftWithMarginsOn(state.ids.axe_bg, 507.0, 523.0),
            },
            SkillIcon::Ability {
                skill: Skill::Axe(AxeSkill::Sunder),
                ability_id: "common.abilities.axe.sunder",
                position: TopLeftWithMarginsOn(state.ids.axe_bg, 387.0, 674.0),
            },
            SkillIcon::Ability {
                skill: Skill::Axe(AxeSkill::Plunder),
                ability_id: "common.abilities.axe.plunder",
                position: TopLeftWithMarginsOn(state.ids.axe_bg, 387.0, 774.0),
            },
            SkillIcon::Ability {
                skill: Skill::Axe(AxeSkill::Defiance),
                ability_id: "common.abilities.axe.defiance",
                position: TopLeftWithMarginsOn(state.ids.axe_bg, 287.0, 624.0),
            },
            SkillIcon::Ability {
                skill: Skill::Axe(AxeSkill::Keelhaul),
                ability_id: "common.abilities.axe.keelhaul",
                position: TopLeftWithMarginsOn(state.ids.axe_bg, 287.0, 824.0),
            },
            SkillIcon::Ability {
                skill: Skill::Axe(AxeSkill::Bulkhead),
                ability_id: "common.abilities.axe.bulkhead",
                position: TopLeftWithMarginsOn(state.ids.axe_bg, 287.0, 724.0),
            },
            SkillIcon::Ability {
                skill: Skill::Axe(AxeSkill::Capsize),
                ability_id: "common.abilities.axe.capsize",
                position: TopLeftWithMarginsOn(state.ids.axe_bg, 104.0, 724.0),
            },
        ];

        state.update(|s| {
            s.ids
                .skills
                .resize(skill_buttons.len(), &mut ui.widget_id_generator())
        });
        state.update(|s| {
            s.ids
                .skill_lock_imgs
                .resize(skill_buttons.len(), &mut ui.widget_id_generator())
        });

        self.handle_skill_buttons(skill_buttons, ui, &mut events, diary_tooltip, state);
        events
    }

    fn handle_hammer_skills_window(
        &mut self,
        diary_tooltip: &Tooltip,
        state: &mut State<DiaryState>,
        ui: &mut UiCell,
        mut events: Vec<Event>,
    ) -> Vec<Event> {
        // Hammer
        Image::new(self.imgs.hammer_bg)
            .wh([924.0, 619.0])
            .mid_top_with_margin_on(state.ids.content_align, 65.0)
            .color(Some(Color::Rgba(1.0, 1.0, 1.0, 1.0)))
            .set(state.ids.hammer_bg, ui);

        use PositionSpecifier::TopLeftWithMarginsOn;
        let skill_buttons = &[
            SkillIcon::Ability {
                skill: Skill::Hammer(HammerSkill::ScornfulSwipe),
                ability_id: "common.abilities.hammer.scornful_swipe",
                position: TopLeftWithMarginsOn(state.ids.hammer_bg, 455.0, 424.0),
            },
            SkillIcon::Ability {
                skill: Skill::Hammer(HammerSkill::Tremor),
                ability_id: "common.abilities.hammer.tremor",
                position: TopLeftWithMarginsOn(state.ids.hammer_bg, 398.0, 172.0),
            },
            SkillIcon::Ability {
                skill: Skill::Hammer(HammerSkill::VigorousBash),
                ability_id: "common.abilities.hammer.vigorous_bash",
                position: TopLeftWithMarginsOn(state.ids.hammer_bg, 398.0, 272.0),
            },
            SkillIcon::Ability {
                skill: Skill::Hammer(HammerSkill::Retaliate),
                ability_id: "common.abilities.hammer.retaliate",
                position: TopLeftWithMarginsOn(state.ids.hammer_bg, 284.0, 122.0),
            },
            SkillIcon::Ability {
                skill: Skill::Hammer(HammerSkill::SpineCracker),
                ability_id: "common.abilities.hammer.spine_cracker",
                position: TopLeftWithMarginsOn(state.ids.hammer_bg, 284.0, 222.0),
            },
            SkillIcon::Ability {
                skill: Skill::Hammer(HammerSkill::Breach),
                ability_id: "common.abilities.hammer.breach",
                position: TopLeftWithMarginsOn(state.ids.hammer_bg, 284.0, 322.0),
            },
            SkillIcon::Ability {
                skill: Skill::Hammer(HammerSkill::IronTempest),
                ability_id: "common.abilities.hammer.iron_tempest",
                position: TopLeftWithMarginsOn(state.ids.hammer_bg, 170.0, 172.0),
            },
            SkillIcon::Ability {
                skill: Skill::Hammer(HammerSkill::Upheaval),
                ability_id: "common.abilities.hammer.upheaval",
                position: TopLeftWithMarginsOn(state.ids.hammer_bg, 170.0, 272.0),
            },
            SkillIcon::Ability {
                skill: Skill::Hammer(HammerSkill::Thunderclap),
                ability_id: "common.abilities.hammer.thunderclap",
                position: TopLeftWithMarginsOn(state.ids.hammer_bg, 56.0, 172.0),
            },
            SkillIcon::Ability {
                skill: Skill::Hammer(HammerSkill::SeismicShock),
                ability_id: "common.abilities.hammer.seismic_shock",
                position: TopLeftWithMarginsOn(state.ids.hammer_bg, 56.0, 272.0),
            },
            SkillIcon::Ability {
                skill: Skill::Hammer(HammerSkill::HeavyWhorl),
                ability_id: "common.abilities.hammer.heavy_whorl",
                position: TopLeftWithMarginsOn(state.ids.hammer_bg, 398.0, 576.0),
            },
            SkillIcon::Ability {
                skill: Skill::Hammer(HammerSkill::Intercept),
                ability_id: "common.abilities.hammer.intercept",
                position: TopLeftWithMarginsOn(state.ids.hammer_bg, 398.0, 676.0),
            },
            SkillIcon::Ability {
                skill: Skill::Hammer(HammerSkill::PileDriver),
                ability_id: "common.abilities.hammer.pile_driver",
                position: TopLeftWithMarginsOn(state.ids.hammer_bg, 284.0, 526.0),
            },
            SkillIcon::Ability {
                skill: Skill::Hammer(HammerSkill::LungPummel),
                ability_id: "common.abilities.hammer.lung_pummel",
                position: TopLeftWithMarginsOn(state.ids.hammer_bg, 284.0, 626.0),
            },
            SkillIcon::Ability {
                skill: Skill::Hammer(HammerSkill::HelmCrusher),
                ability_id: "common.abilities.hammer.helm_crusher",
                position: TopLeftWithMarginsOn(state.ids.hammer_bg, 284.0, 726.0),
            },
            SkillIcon::Ability {
                skill: Skill::Hammer(HammerSkill::Rampart),
                ability_id: "common.abilities.hammer.rampart",
                position: TopLeftWithMarginsOn(state.ids.hammer_bg, 170.0, 576.0),
            },
            SkillIcon::Ability {
                skill: Skill::Hammer(HammerSkill::Tenacity),
                ability_id: "common.abilities.hammer.tenacity",
                position: TopLeftWithMarginsOn(state.ids.hammer_bg, 170.0, 676.0),
            },
            SkillIcon::Ability {
                skill: Skill::Hammer(HammerSkill::Earthshaker),
                ability_id: "common.abilities.hammer.earthshaker",
                position: TopLeftWithMarginsOn(state.ids.hammer_bg, 56.0, 576.0),
            },
            SkillIcon::Ability {
                skill: Skill::Hammer(HammerSkill::Judgement),
                ability_id: "common.abilities.hammer.judgement",
                position: TopLeftWithMarginsOn(state.ids.hammer_bg, 56.0, 676.0),
            },
        ];

        state.update(|s| {
            s.ids
                .skills
                .resize(skill_buttons.len(), &mut ui.widget_id_generator())
        });
        state.update(|s| {
            s.ids
                .skill_lock_imgs
                .resize(skill_buttons.len(), &mut ui.widget_id_generator())
        });

        self.handle_skill_buttons(skill_buttons, ui, &mut events, diary_tooltip, state);
        events
    }

    fn handle_sceptre_skills_window(
        &mut self,
        diary_tooltip: &Tooltip,
        state: &mut State<DiaryState>,
        ui: &mut UiCell,
        mut events: Vec<Event>,
    ) -> Vec<Event> {
        // Title text
        let tree_title = &self.localized_strings.get_msg("common-weapons-sceptre");

        Text::new(tree_title)
            .mid_top_with_margin_on(state.ids.content_align, 2.0)
            .font_id(self.fonts.cyri.conrod_id)
            .font_size(self.fonts.cyri.scale(34))
            .color(TEXT_COLOR)
            .set(state.ids.tree_title_txt, ui);

        // Number of skills per rectangle per weapon, start counting at 0
        // Maximum of 9 skills/8 indices
        let skills_top_l = 5;
        let skills_top_r = 5;
        let skills_bot_l = 5;
        let skills_bot_r = 0;

        self.setup_state_for_skill_icons(
            state,
            ui,
            skills_top_l,
            skills_top_r,
            skills_bot_l,
            skills_bot_r,
        );

        // Skill icons and buttons
        use skills::SceptreSkill::*;
        // Sceptre
        Image::new(animate_by_pulse(
            &self
                .item_imgs
                .img_ids_or_not_found_img(ItemKey::Simple("example_sceptre".to_string())),
            self.pulse,
        ))
        .wh(ART_SIZE)
        .middle_of(state.ids.content_align)
        .color(Some(Color::Rgba(1.0, 1.0, 1.0, 1.0)))
        .set(state.ids.sceptre_render, ui);
        use PositionSpecifier::MidTopWithMarginOn;
        let skill_buttons = &[
            // Top Left skills
            //        5 1 6
            //        3 0 4
            //        8 2 7
            SkillIcon::Descriptive {
                title: "hud-skill-sc_lifesteal_title",
                desc: "hud-skill-sc_lifesteal",
                image: self.imgs.skill_sceptre_lifesteal,
                position: MidTopWithMarginOn(state.ids.skills_top_l[0], 3.0),
                id: state.ids.skill_sceptre_lifesteal_0,
            },
            SkillIcon::Unlockable {
                skill: Skill::Sceptre(LDamage),
                image: self.imgs.magic_damage_skill,
                position: MidTopWithMarginOn(state.ids.skills_top_l[1], 3.0),
                id: state.ids.skill_sceptre_lifesteal_1,
            },
            SkillIcon::Unlockable {
                skill: Skill::Sceptre(LRange),
                image: self.imgs.magic_distance_skill,
                position: MidTopWithMarginOn(state.ids.skills_top_l[2], 3.0),
                id: state.ids.skill_sceptre_lifesteal_2,
            },
            SkillIcon::Unlockable {
                skill: Skill::Sceptre(LLifesteal),
                image: self.imgs.magic_lifesteal_skill,
                position: MidTopWithMarginOn(state.ids.skills_top_l[3], 3.0),
                id: state.ids.skill_sceptre_lifesteal_3,
            },
            SkillIcon::Unlockable {
                skill: Skill::Sceptre(LRegen),
                image: self.imgs.magic_energy_regen_skill,
                position: MidTopWithMarginOn(state.ids.skills_top_l[4], 3.0),
                id: state.ids.skill_sceptre_lifesteal_4,
            },
            // Top right skills
            SkillIcon::Descriptive {
                title: "hud-skill-sc_heal_title",
                desc: "hud-skill-sc_heal",
                image: self.imgs.skill_sceptre_heal,
                position: MidTopWithMarginOn(state.ids.skills_top_r[0], 3.0),
                id: state.ids.skill_sceptre_heal_0,
            },
            SkillIcon::Unlockable {
                skill: Skill::Sceptre(HHeal),
                image: self.imgs.heal_heal_skill,
                position: MidTopWithMarginOn(state.ids.skills_top_r[1], 3.0),
                id: state.ids.skill_sceptre_heal_1,
            },
            SkillIcon::Unlockable {
                skill: Skill::Sceptre(HDuration),
                image: self.imgs.heal_duration_skill,
                position: MidTopWithMarginOn(state.ids.skills_top_r[2], 3.0),
                id: state.ids.skill_sceptre_heal_2,
            },
            SkillIcon::Unlockable {
                skill: Skill::Sceptre(HRange),
                image: self.imgs.heal_radius_skill,
                position: MidTopWithMarginOn(state.ids.skills_top_r[3], 3.0),
                id: state.ids.skill_sceptre_heal_3,
            },
            SkillIcon::Unlockable {
                skill: Skill::Sceptre(HCost),
                image: self.imgs.heal_cost_skill,
                position: MidTopWithMarginOn(state.ids.skills_top_r[4], 3.0),
                id: state.ids.skill_sceptre_heal_4,
            },
            // Bottom left skills
            SkillIcon::Unlockable {
                skill: Skill::Sceptre(UnlockAura),
                image: self.imgs.skill_sceptre_aura,
                position: MidTopWithMarginOn(state.ids.skills_bot_l[0], 3.0),
                id: state.ids.skill_sceptre_aura_0,
            },
            SkillIcon::Unlockable {
                skill: Skill::Sceptre(AStrength),
                image: self.imgs.buff_damage_skill,
                position: MidTopWithMarginOn(state.ids.skills_bot_l[1], 3.0),
                id: state.ids.skill_sceptre_aura_1,
            },
            SkillIcon::Unlockable {
                skill: Skill::Sceptre(ADuration),
                image: self.imgs.buff_duration_skill,
                position: MidTopWithMarginOn(state.ids.skills_bot_l[2], 3.0),
                id: state.ids.skill_sceptre_aura_2,
            },
            SkillIcon::Unlockable {
                skill: Skill::Sceptre(ARange),
                image: self.imgs.buff_radius_skill,
                position: MidTopWithMarginOn(state.ids.skills_bot_l[3], 3.0),
                id: state.ids.skill_sceptre_aura_3,
            },
            SkillIcon::Unlockable {
                skill: Skill::Sceptre(ACost),
                image: self.imgs.buff_cost_skill,
                position: MidTopWithMarginOn(state.ids.skills_bot_l[4], 3.0),
                id: state.ids.skill_sceptre_aura_4,
            },
        ];

        self.handle_skill_buttons(skill_buttons, ui, &mut events, diary_tooltip, state);
        events
    }

    fn handle_bow_skills_window(
        &mut self,
        diary_tooltip: &Tooltip,
        state: &mut State<DiaryState>,
        ui: &mut UiCell,
        mut events: Vec<Event>,
    ) -> Vec<Event> {
        Image::new(self.imgs.bow_bg)
            .wh([924.0, 619.0])
            .mid_top_with_margin_on(state.ids.content_align, 65.0)
            .color(Some(Color::Rgba(1.0, 1.0, 1.0, 1.0)))
            .set(state.ids.bow_bg, ui);

        use PositionSpecifier::TopLeftWithMarginsOn;
        let skill_buttons = &[
            SkillIcon::Ability {
                skill: Skill::Bow(BowSkill::Foothold),
                ability_id: "common.abilities.bow.foothold",
                position: TopLeftWithMarginsOn(state.ids.bow_bg, 424.0, 368.0),
            },
            SkillIcon::Ability {
                skill: Skill::Bow(BowSkill::HeavyNock),
                ability_id: "common.abilities.bow.heavy_nock",
                position: TopLeftWithMarginsOn(state.ids.bow_bg, 424.0, 480.0),
            },
            SkillIcon::Ability {
                skill: Skill::Bow(BowSkill::ArdentHunt),
                ability_id: "common.abilities.bow.ardent_hunt",
                position: TopLeftWithMarginsOn(state.ids.bow_bg, 310.0, 204.0),
            },
            SkillIcon::Ability {
                skill: Skill::Bow(BowSkill::SepticShot),
                ability_id: "common.abilities.bow.septic_shot",
                position: TopLeftWithMarginsOn(state.ids.bow_bg, 310.0, 424.0),
            },
            SkillIcon::Ability {
                skill: Skill::Bow(BowSkill::Barrage),
                ability_id: "common.abilities.bow.barrage",
                position: TopLeftWithMarginsOn(state.ids.bow_bg, 310.0, 644.0),
            },
            SkillIcon::Ability {
                skill: Skill::Bow(BowSkill::StormChaser),
                ability_id: "common.abilities.bow.storm_chaser",
                position: TopLeftWithMarginsOn(state.ids.bow_bg, 196.0, 154.0),
            },
            SkillIcon::Ability {
                skill: Skill::Bow(BowSkill::EagleEye),
                ability_id: "common.abilities.bow.eagle_eye",
                position: TopLeftWithMarginsOn(state.ids.bow_bg, 196.0, 254.0),
            },
            SkillIcon::Ability {
                skill: Skill::Bow(BowSkill::IgniteArrow),
                ability_id: "common.abilities.bow.ignite_arrow",
                position: TopLeftWithMarginsOn(state.ids.bow_bg, 196.0, 374.0),
            },
            SkillIcon::Ability {
                skill: Skill::Bow(BowSkill::DrenchArrow),
                ability_id: "common.abilities.bow.drench_arrow",
                position: TopLeftWithMarginsOn(state.ids.bow_bg, 196.0, 474.0),
            },
            SkillIcon::Ability {
                skill: Skill::Bow(BowSkill::PiercingGale),
                ability_id: "common.abilities.bow.piercing_gale",
                position: TopLeftWithMarginsOn(state.ids.bow_bg, 196.0, 594.0),
            },
            SkillIcon::Ability {
                skill: Skill::Bow(BowSkill::ThornStake),
                ability_id: "common.abilities.bow.thorn_stake",
                position: TopLeftWithMarginsOn(state.ids.bow_bg, 196.0, 694.0),
            },
            SkillIcon::Ability {
                skill: Skill::Bow(BowSkill::Heartseeker),
                ability_id: "common.abilities.bow.heartseeker",
                position: TopLeftWithMarginsOn(state.ids.bow_bg, 82.0, 154.0),
            },
            SkillIcon::Ability {
                skill: Skill::Bow(BowSkill::Hawkstrike),
                ability_id: "common.abilities.bow.hawkstrike",
                position: TopLeftWithMarginsOn(state.ids.bow_bg, 82.0, 254.0),
            },
            SkillIcon::Ability {
                skill: Skill::Bow(BowSkill::FreezeArrow),
                ability_id: "common.abilities.bow.freeze_arrow",
                position: TopLeftWithMarginsOn(state.ids.bow_bg, 82.0, 374.0),
            },
            SkillIcon::Ability {
                skill: Skill::Bow(BowSkill::JoltArrow),
                ability_id: "common.abilities.bow.jolt_arrow",
                position: TopLeftWithMarginsOn(state.ids.bow_bg, 82.0, 474.0),
            },
            SkillIcon::Ability {
                skill: Skill::Bow(BowSkill::Fusillade),
                ability_id: "common.abilities.bow.fusillade",
                position: TopLeftWithMarginsOn(state.ids.bow_bg, 82.0, 594.0),
            },
            SkillIcon::Ability {
                skill: Skill::Bow(BowSkill::DeathVolley),
                ability_id: "common.abilities.bow.death_volley",
                position: TopLeftWithMarginsOn(state.ids.bow_bg, 82.0, 694.0),
            },
        ];

        state.update(|s| {
            s.ids
                .skills
                .resize(skill_buttons.len(), &mut ui.widget_id_generator())
        });
        state.update(|s| {
            s.ids
                .skill_lock_imgs
                .resize(skill_buttons.len(), &mut ui.widget_id_generator())
        });

        self.handle_skill_buttons(skill_buttons, ui, &mut events, diary_tooltip, state);
        events
    }

    fn handle_staff_skills_window(
        &mut self,
        diary_tooltip: &Tooltip,
        state: &mut State<DiaryState>,
        ui: &mut UiCell,
        mut events: Vec<Event>,
    ) -> Vec<Event> {
        use skills::StaffSkill::*;

        Image::new(self.imgs.staff_bg)
            .wh([924.0, 619.0])
            .mid_top_with_margin_on(state.ids.content_align, 65.0)
            .color(Some(Color::Rgba(1.0, 1.0, 1.0, 1.0)))
            .set(state.ids.staff_bg, ui);

        use PositionSpecifier::TopLeftWithMarginsOn;
        let skill_buttons = &[
            SkillIcon::Ability {
                skill: Skill::Staff(FireShockwave),
                ability_id: "common.abilities.staff.fireshockwave",
                position: TopLeftWithMarginsOn(state.ids.staff_bg, 436.0, 421.0),
            },
            SkillIcon::Ability {
                skill: Skill::Staff(FireDash),
                ability_id: "common.abilities.staff.fire_dash",
                position: TopLeftWithMarginsOn(state.ids.staff_bg, 319.0, 331.0),
            },
            SkillIcon::Ability {
                skill: Skill::Staff(FlameCloak),
                ability_id: "common.abilities.staff.flame_cloak",
                position: TopLeftWithMarginsOn(state.ids.staff_bg, 319.0, 515.0),
            },
            SkillIcon::Ability {
                skill: Skill::Staff(FireBreath),
                ability_id: "common.abilities.staff.fire_breath",
                position: TopLeftWithMarginsOn(state.ids.staff_bg, 200.0, 515.0),
            },
            SkillIcon::Ability {
                skill: Skill::Staff(NapalmStrike),
                ability_id: "common.abilities.staff.napalm_strike",
                position: TopLeftWithMarginsOn(state.ids.staff_bg, 200.0, 331.0),
            },
            SkillIcon::Ability {
                skill: Skill::Staff(Pyroclasm),
                ability_id: "common.abilities.staff.pyroclasm",
                position: TopLeftWithMarginsOn(state.ids.staff_bg, 86.0, 422.0),
            },
        ];

        state.update(|s| {
            s.ids
                .skills
                .resize(skill_buttons.len(), &mut ui.widget_id_generator())
        });
        state.update(|s| {
            s.ids
                .skill_lock_imgs
                .resize(skill_buttons.len(), &mut ui.widget_id_generator())
        });

        self.handle_skill_buttons(skill_buttons, ui, &mut events, diary_tooltip, state);
        events
    }

    /// The martial-role Staff's own skill tree, kept separate from
    /// `handle_staff_skills_window` (the caster/fire tree) above — the two
    /// share a background asset (no bespoke art authored for this tree yet)
    /// but resolve distinct `SkillGroupKind`s and never share progress.
    /// Layout: two T1 roots (`Sweep`, `Brace`) each with a T2 follow-up,
    /// converging on the `Avalanche` T3 capstone.
    fn handle_staff_martial_skills_window(
        &mut self,
        diary_tooltip: &Tooltip,
        state: &mut State<DiaryState>,
        ui: &mut UiCell,
        mut events: Vec<Event>,
    ) -> Vec<Event> {
        use skills::StaffMartialSkill::*;

        Image::new(self.imgs.staff_bg)
            .wh([924.0, 619.0])
            .mid_top_with_margin_on(state.ids.content_align, 65.0)
            .color(Some(Color::Rgba(1.0, 1.0, 1.0, 1.0)))
            .set(state.ids.staff_bg, ui);

        use PositionSpecifier::TopLeftWithMarginsOn;
        let skill_buttons = &[
            SkillIcon::Ability {
                skill: Skill::StaffMartial(Sweep),
                ability_id: "common.abilities.staff_martial.sweep",
                position: TopLeftWithMarginsOn(state.ids.staff_bg, 460.0, 100.0),
            },
            SkillIcon::Ability {
                skill: Skill::StaffMartial(Brace),
                ability_id: "common.abilities.staff_martial.brace",
                position: TopLeftWithMarginsOn(state.ids.staff_bg, 160.0, 100.0),
            },
            SkillIcon::Ability {
                skill: Skill::StaffMartial(WhirlingGale),
                ability_id: "common.abilities.staff_martial.whirling_gale",
                position: TopLeftWithMarginsOn(state.ids.staff_bg, 460.0, 350.0),
            },
            SkillIcon::Ability {
                skill: Skill::StaffMartial(GlacialThrust),
                ability_id: "common.abilities.staff_martial.glacial_thrust",
                position: TopLeftWithMarginsOn(state.ids.staff_bg, 160.0, 350.0),
            },
            SkillIcon::Ability {
                skill: Skill::StaffMartial(Avalanche),
                ability_id: "common.abilities.staff_martial.avalanche",
                position: TopLeftWithMarginsOn(state.ids.staff_bg, 310.0, 600.0),
            },
        ];

        state.update(|s| {
            s.ids
                .skills
                .resize(skill_buttons.len(), &mut ui.widget_id_generator())
        });
        state.update(|s| {
            s.ids
                .skill_lock_imgs
                .resize(skill_buttons.len(), &mut ui.widget_id_generator())
        });

        self.handle_skill_buttons(skill_buttons, ui, &mut events, diary_tooltip, state);
        events
    }

    fn handle_mining_skills_window(
        &mut self,
        diary_tooltip: &Tooltip,
        state: &mut State<DiaryState>,
        ui: &mut UiCell,
        mut events: Vec<Event>,
    ) -> Vec<Event> {
        // Title text
        let tree_title = &self.localized_strings.get_msg("common-tool-mining");

        Text::new(tree_title)
            .mid_top_with_margin_on(state.ids.content_align, 2.0)
            .font_id(self.fonts.cyri.conrod_id)
            .font_size(self.fonts.cyri.scale(34))
            .color(TEXT_COLOR)
            .set(state.ids.tree_title_txt, ui);

        // Number of skills per rectangle per weapon, start counting at 0
        // Maximum of 9 skills/8 indices
        let skills_top_l = 4;
        let skills_top_r = 0;
        let skills_bot_l = 0;
        let skills_bot_r = 0;

        self.setup_state_for_skill_icons(
            state,
            ui,
            skills_top_l,
            skills_top_r,
            skills_bot_l,
            skills_bot_r,
        );

        // Skill icons and buttons
        use skills::MiningSkill::*;
        // Mining
        Image::new(animate_by_pulse(
            &self
                .item_imgs
                .img_ids_or_not_found_img(ItemKey::Simple("example_pick".to_string())),
            self.pulse,
        ))
        .wh(ART_SIZE)
        .middle_of(state.ids.content_align)
        .color(Some(Color::Rgba(1.0, 1.0, 1.0, 1.0)))
        .set(state.ids.pick_render, ui);

        use PositionSpecifier::MidTopWithMarginOn;
        let skill_buttons = &[
            // Top Left skills
            //        5 1 6
            //        3 0 4
            //        8 2 7
            SkillIcon::Descriptive {
                title: "hud-skill-pick_strike_title",
                desc: "hud-skill-pick_strike",
                image: self.imgs.pickaxe,
                position: MidTopWithMarginOn(state.ids.skills_top_l[0], 3.0),
                id: state.ids.skill_pick_m1,
            },
            SkillIcon::Unlockable {
                skill: Skill::Pick(Speed),
                image: self.imgs.pickaxe_speed_skill,
                position: MidTopWithMarginOn(state.ids.skills_top_l[1], 3.0),
                id: state.ids.skill_pick_m1_0,
            },
            SkillIcon::Unlockable {
                skill: Skill::Pick(OreGain),
                image: self.imgs.pickaxe_oregain_skill,
                position: MidTopWithMarginOn(state.ids.skills_top_l[2], 3.0),
                id: state.ids.skill_pick_m1_1,
            },
            SkillIcon::Unlockable {
                skill: Skill::Pick(GemGain),
                image: self.imgs.pickaxe_gemgain_skill,
                position: MidTopWithMarginOn(state.ids.skills_top_l[3], 3.0),
                id: state.ids.skill_pick_m1_2,
            },
        ];

        self.handle_skill_buttons(skill_buttons, ui, &mut events, diary_tooltip, state);
        events
    }

    fn handle_skill_buttons(
        &mut self,
        icons: &[SkillIcon],
        ui: &mut UiCell,
        events: &mut Vec<Event>,
        diary_tooltip: &Tooltip,
        state: &mut State<DiaryState>,
    ) {
        for (i, icon) in icons.iter().enumerate() {
            match icon {
                SkillIcon::Descriptive {
                    title,
                    desc,
                    image,
                    position,
                    id,
                } => {
                    // TODO: shouldn't this be a `Image::new`?
                    Button::image(*image)
                        .w_h(74.0, 74.0)
                        .position(*position)
                        .with_tooltip(
                            self.tooltip_manager,
                            &self.localized_strings.get_msg(title),
                            &self.localized_strings.get_msg(desc),
                            diary_tooltip,
                            TEXT_COLOR,
                        )
                        .set(*id, ui);
                },
                SkillIcon::Unlockable {
                    skill,
                    image,
                    position,
                    id,
                } => self.create_unlock_skill_button(
                    *skill,
                    *image,
                    *position,
                    *id,
                    ui,
                    events,
                    diary_tooltip,
                ),
                SkillIcon::Ability {
                    skill,
                    ability_id,
                    position,
                } => self.create_unlock_ability_button(
                    *skill,
                    ability_id,
                    *position,
                    i,
                    ui,
                    events,
                    diary_tooltip,
                    state,
                ),
            }
        }
    }

    fn setup_state_for_skill_icons(
        &mut self,
        state: &mut State<DiaryState>,
        ui: &mut UiCell,
        skills_top_l: usize,
        skills_top_r: usize,
        skills_bot_l: usize,
        skills_bot_r: usize,
    ) {
        // Update widget id array len
        state.update(|s| {
            s.ids
                .skills_top_l
                .resize(skills_top_l, &mut ui.widget_id_generator())
        });
        state.update(|s| {
            s.ids
                .skills_top_r
                .resize(skills_top_r, &mut ui.widget_id_generator())
        });
        state.update(|s| {
            s.ids
                .skills_bot_l
                .resize(skills_bot_l, &mut ui.widget_id_generator())
        });
        state.update(|s| {
            s.ids
                .skills_bot_r
                .resize(skills_bot_r, &mut ui.widget_id_generator())
        });

        // Create Background Images to place skill icons on them later
        // Create central skill first, others around it:
        //
        //        5 1 6
        //        3 0 4
        //        8 2 7
        //
        //
        let offset_0 = 22.0;
        let offset_1 = -122.0;
        let offset_2 = offset_1 - -20.0;

        let skill_pos = |idx, align, central_skill| {
            use PositionSpecifier::*;
            match idx {
                // Central skill
                0 => MiddleOf(align),
                // 12:00
                1 => UpFrom(central_skill, offset_0),
                // 6:00
                2 => DownFrom(central_skill, offset_0),
                // 3:00
                3 => LeftFrom(central_skill, offset_0),
                // 9:00
                4 => RightFrom(central_skill, offset_0),
                // 10:30
                5 => TopLeftWithMarginsOn(central_skill, offset_1, offset_2),
                // 1:30
                6 => TopRightWithMarginsOn(central_skill, offset_1, offset_2),
                // 4:30
                7 => BottomLeftWithMarginsOn(central_skill, offset_1, offset_2),
                // 7:30
                8 => BottomRightWithMarginsOn(central_skill, offset_1, offset_2),
                buttons => {
                    panic!("{} > 8 position number", buttons);
                },
            }
        };

        // TOP-LEFT Skills
        //
        // TODO: Why this uses while loop on field of struct and not just
        // `for i in 0..skils_top_l`?
        while self.created_btns_top_l < skills_top_l {
            let pos = skill_pos(
                self.created_btns_top_l,
                state.ids.skills_top_l_align,
                state.ids.skills_top_l[0],
            );
            Button::image(self.imgs.wpn_icon_border_skills)
                .w_h(80.0, 100.0)
                .position(pos)
                .set(state.ids.skills_top_l[self.created_btns_top_l], ui);
            self.created_btns_top_l += 1;
        }
        // TOP-RIGHT Skills
        while self.created_btns_top_r < skills_top_r {
            let pos = skill_pos(
                self.created_btns_top_r,
                state.ids.skills_top_r_align,
                state.ids.skills_top_r[0],
            );
            Button::image(self.imgs.wpn_icon_border_skills)
                .w_h(80.0, 100.0)
                .position(pos)
                .set(state.ids.skills_top_r[self.created_btns_top_r], ui);
            self.created_btns_top_r += 1;
        }
        // BOTTOM-LEFT Skills
        while self.created_btns_bot_l < skills_bot_l {
            let pos = skill_pos(
                self.created_btns_bot_l,
                state.ids.skills_bot_l_align,
                state.ids.skills_bot_l[0],
            );
            Button::image(self.imgs.wpn_icon_border_skills)
                .w_h(80.0, 100.0)
                .position(pos)
                .set(state.ids.skills_bot_l[self.created_btns_bot_l], ui);
            self.created_btns_bot_l += 1;
        }
        // BOTTOM-RIGHT Skills
        while self.created_btns_bot_r < skills_bot_r {
            let pos = skill_pos(
                self.created_btns_bot_r,
                state.ids.skills_bot_r_align,
                state.ids.skills_bot_r[0],
            );
            Button::image(self.imgs.wpn_icon_border_skills)
                .w_h(80.0, 100.0)
                .position(pos)
                .set(state.ids.skills_bot_r[self.created_btns_bot_r], ui);
            self.created_btns_bot_r += 1;
        }
    }

    fn create_unlock_skill_button(
        &mut self,
        skill: Skill,
        skill_image: image::Id,
        position: PositionSpecifier,
        widget_id: widget::Id,
        ui: &mut UiCell,
        events: &mut Vec<Event>,
        diary_tooltip: &Tooltip,
    ) {
        let label = if self.skill_set.prerequisites_met(skill) {
            let current = self.skill_set.skill_level(skill).unwrap_or(0);
            let max = skill.max_level();
            format!("{}/{}", current, max)
        } else {
            "".to_owned()
        };

        let label_color = if self.skill_set.is_at_max_level(skill) {
            TEXT_COLOR
        } else if self.skill_set.sufficient_skill_points(skill) {
            HP_COLOR
        } else {
            CRITICAL_HP_COLOR
        };

        let image_color = if self.skill_set.prerequisites_met(skill) {
            TEXT_COLOR
        } else {
            Color::Rgba(0.41, 0.41, 0.41, 0.7)
        };

        let skill_strings = skill_strings(skill);
        let (title, description) =
            skill_strings.localize(self.localized_strings, self.skill_set, skill);
        // BL-06 UX: append the next-rank SP cost to the tooltip so the red
        // "insufficient points" colour is self-explanatory (cost scales with rank:
        // rank N costs N SP). Mirrors the active-ability button. Omitted at max level.
        let description = if self.skill_set.is_at_max_level(skill) {
            format!("{description}")
        } else {
            format!(
                "{description}\n{}",
                sp(self.localized_strings, self.skill_set, skill)
            )
        };

        let button = Button::image(skill_image)
            .w_h(74.0, 74.0)
            .position(position)
            .label(&label)
            .label_y(conrod_core::position::Relative::Scalar(-47.0))
            .label_x(conrod_core::position::Relative::Scalar(0.0))
            .label_color(label_color)
            .label_font_size(self.fonts.cyri.scale(15))
            .label_font_id(self.fonts.cyri.conrod_id)
            .image_color(image_color)
            .with_tooltip(
                self.tooltip_manager,
                &title,
                &description,
                diary_tooltip,
                TEXT_COLOR,
            )
            .set(widget_id, ui);

        if button.was_clicked() {
            events.push(Event::UnlockSkill(skill));
        };
    }

    fn create_unlock_ability_button(
        &mut self,
        skill: Skill,
        ability_id: &str,
        position: PositionSpecifier,
        widget_index: usize,
        ui: &mut UiCell,
        events: &mut Vec<Event>,
        diary_tooltip: &Tooltip,
        state: &mut State<DiaryState>,
    ) {
        let locked = !self.skill_set.prerequisites_met(skill);
        let owned = self.skill_set.has_skill(skill);
        let image_color = if owned {
            TEXT_COLOR
        } else {
            Color::Rgba(0.41, 0.41, 0.41, 0.7)
        };

        let (title, description) = util::ability_description(ability_id, self.localized_strings);

        let sp_cost = sp(self.localized_strings, self.skill_set, skill);

        let description = format!("{description}\n{sp_cost}");

        let button = Button::image(util::ability_image(self.imgs, ability_id))
            .w_h(76.0, 76.0)
            .position(position)
            .image_color(image_color)
            .with_tooltip(
                self.tooltip_manager,
                &title,
                &description,
                diary_tooltip,
                TEXT_COLOR,
            )
            .set(state.ids.skills[widget_index], ui);

        // Lock Image
        if locked {
            Image::new(self.imgs.lock)
                .w_h(76.0, 76.0)
                .middle_of(state.ids.skills[widget_index])
                .graphics_for(state.ids.skills[widget_index])
                .color(Some(Color::Rgba(1.0, 1.0, 1.0, 0.8)))
                .set(state.ids.skill_lock_imgs[widget_index], ui);
        }

        if button.was_clicked() {
            events.push(Event::UnlockSkill(skill));
        };
    }
}

/// Returns skill info as a tuple of title and description.
///
/// If you want to get localized version, use `SkillStrings::localize` method
fn skill_strings(skill: Skill) -> SkillStrings<'static> {
    match skill {
        // general tree
        Skill::UnlockGroup(s) => unlock_skill_strings(s),
        // weapon trees
        Skill::Sceptre(s) => sceptre_skill_strings(s),
        // movement trees
        Skill::Climb(s) => climb_skill_strings(s),
        Skill::Swim(s) => swim_skill_strings(s),
        // mining
        Skill::Pick(s) => mining_skill_strings(s),
        // BL-06 P3a: class skill trees
        Skill::Warrior(s) => warrior_skill_strings(s),
        Skill::Mage(s) => mage_skill_strings(s),
        Skill::Cleric(s) => cleric_skill_strings(s),
        Skill::Rogue(s) => rogue_skill_strings(s),
        _ => SkillStrings::plain("", ""),
    }
}

fn warrior_skill_strings(skill: WarriorSkill) -> SkillStrings<'static> {
    match skill {
        WarriorSkill::HardenedBody => SkillStrings::plain(
            "hud-skill-class-warrior-hardened_body_title",
            "hud-skill-class-warrior-hardened_body",
        ),
        WarriorSkill::PracticedStrikes => SkillStrings::plain(
            "hud-skill-class-warrior-practiced_strikes_title",
            "hud-skill-class-warrior-practiced_strikes",
        ),
        WarriorSkill::Rally => SkillStrings::plain(
            "hud-skill-class-warrior-rally_title",
            "hud-skill-class-warrior-rally",
        ),
        WarriorSkill::IronSkin => SkillStrings::plain(
            "hud-skill-class-warrior-iron_skin_title",
            "hud-skill-class-warrior-iron_skin",
        ),
        WarriorSkill::BrutalEdge => SkillStrings::plain(
            "hud-skill-class-warrior-brutal_edge_title",
            "hud-skill-class-warrior-brutal_edge",
        ),
        WarriorSkill::CrushingBlows => SkillStrings::plain(
            "hud-skill-class-warrior-crushing_blows_title",
            "hud-skill-class-warrior-crushing_blows",
        ),
        WarriorSkill::Stalwart => SkillStrings::plain(
            "hud-skill-class-warrior-stalwart_title",
            "hud-skill-class-warrior-stalwart",
        ),
        WarriorSkill::SunderingForce => SkillStrings::plain(
            "hud-skill-class-warrior-sundering_force_title",
            "hud-skill-class-warrior-sundering_force",
        ),
        WarriorSkill::Stagger => SkillStrings::plain(
            "hud-skill-class-warrior-stagger_title",
            "hud-skill-class-warrior-stagger",
        ),
        WarriorSkill::BattleMomentum => SkillStrings::plain(
            "hud-skill-class-warrior-battle_momentum_title",
            "hud-skill-class-warrior-battle_momentum",
        ),
        WarriorSkill::BulwarkStance => SkillStrings::plain(
            "hud-skill-class-warrior-bulwark_stance_title",
            "hud-skill-class-warrior-bulwark_stance",
        ),
        WarriorSkill::Onslaught => SkillStrings::plain(
            "hud-skill-class-warrior-onslaught_title",
            "hud-skill-class-warrior-onslaught",
        ),
    }
}

fn mage_skill_strings(skill: MageSkill) -> SkillStrings<'static> {
    match skill {
        MageSkill::FocusedMind => SkillStrings::plain(
            "hud-skill-class-mage-focused_mind_title",
            "hud-skill-class-mage-focused_mind",
        ),
        MageSkill::TrueAim => SkillStrings::plain(
            "hud-skill-class-mage-true_aim_title",
            "hud-skill-class-mage-true_aim",
        ),
        MageSkill::ArcaneSurge => SkillStrings::plain(
            "hud-skill-class-mage-arcane_surge_title",
            "hud-skill-class-mage-arcane_surge",
        ),
        MageSkill::SpellPotency => SkillStrings::plain(
            "hud-skill-class-mage-spell_potency_title",
            "hud-skill-class-mage-spell_potency",
        ),
        MageSkill::PyromanticAttunement => SkillStrings::plain(
            "hud-skill-class-mage-pyromantic_attunement_title",
            "hud-skill-class-mage-pyromantic_attunement",
        ),
        MageSkill::CryomanticAttunement => SkillStrings::plain(
            "hud-skill-class-mage-cryomantic_attunement_title",
            "hud-skill-class-mage-cryomantic_attunement",
        ),
        MageSkill::QuickCasting => SkillStrings::plain(
            "hud-skill-class-mage-quick_casting_title",
            "hud-skill-class-mage-quick_casting",
        ),
        MageSkill::PenetratingMagic => SkillStrings::plain(
            "hud-skill-class-mage-penetrating_magic_title",
            "hud-skill-class-mage-penetrating_magic",
        ),
        MageSkill::WardedSkin => SkillStrings::plain(
            "hud-skill-class-mage-warded_skin_title",
            "hud-skill-class-mage-warded_skin",
        ),
        MageSkill::ManaEfficiency => SkillStrings::plain(
            "hud-skill-class-mage-mana_efficiency_title",
            "hud-skill-class-mage-mana_efficiency",
        ),
        MageSkill::ManaRecover => SkillStrings::plain(
            "hud-skill-class-mage-mana_recover_title",
            "hud-skill-class-mage-mana_recover",
        ),
        MageSkill::ManaFlow => SkillStrings::plain(
            "hud-skill-class-mage-mana_flow_title",
            "hud-skill-class-mage-mana_flow",
        ),
        MageSkill::ArcaneVigor => SkillStrings::plain(
            "hud-skill-class-mage-arcane_vigor_title",
            "hud-skill-class-mage-arcane_vigor",
        ),
        MageSkill::Polyglot => SkillStrings::plain(
            "hud-skill-class-mage-polyglot_title",
            "hud-skill-class-mage-polyglot",
        ),
        MageSkill::Overcharge => SkillStrings::plain(
            "hud-skill-class-mage-overcharge_title",
            "hud-skill-class-mage-overcharge",
        ),
        MageSkill::ArcaneMastery => SkillStrings::plain(
            "hud-skill-class-mage-arcane_mastery_title",
            "hud-skill-class-mage-arcane_mastery",
        ),
    }
}

fn cleric_skill_strings(skill: ClericSkill) -> SkillStrings<'static> {
    match skill {
        ClericSkill::FaithfulVigor => SkillStrings::plain(
            "hud-skill-class-cleric-faithful_vigor_title",
            "hud-skill-class-cleric-faithful_vigor",
        ),
        ClericSkill::DevoutFocus => SkillStrings::plain(
            "hud-skill-class-cleric-devout_focus_title",
            "hud-skill-class-cleric-devout_focus",
        ),
        ClericSkill::MendingLight => SkillStrings::plain(
            "hud-skill-class-cleric-mending_light_title",
            "hud-skill-class-cleric-mending_light",
        ),
        ClericSkill::BlessedAim => SkillStrings::plain(
            "hud-skill-class-cleric-blessed_aim_title",
            "hud-skill-class-cleric-blessed_aim",
        ),
        ClericSkill::SacredWards => SkillStrings::plain(
            "hud-skill-class-cleric-sacred_wards_title",
            "hud-skill-class-cleric-sacred_wards",
        ),
        ClericSkill::SteadfastFaith => SkillStrings::plain(
            "hud-skill-class-cleric-steadfast_faith_title",
            "hud-skill-class-cleric-steadfast_faith",
        ),
        ClericSkill::PurifyingGrace => SkillStrings::plain(
            "hud-skill-class-cleric-purifying_grace_title",
            "hud-skill-class-cleric-purifying_grace",
        ),
        ClericSkill::DivineConduit => SkillStrings::plain(
            "hud-skill-class-cleric-divine_conduit_title",
            "hud-skill-class-cleric-divine_conduit",
        ),
        ClericSkill::SmitingStrikes => SkillStrings::plain(
            "hud-skill-class-cleric-smiting_strikes_title",
            "hud-skill-class-cleric-smiting_strikes",
        ),
        ClericSkill::ArmorOfFaith => SkillStrings::plain(
            "hud-skill-class-cleric-armor_of_faith_title",
            "hud-skill-class-cleric-armor_of_faith",
        ),
        ClericSkill::Aegis => SkillStrings::plain(
            "hud-skill-class-cleric-aegis_title",
            "hud-skill-class-cleric-aegis",
        ),
        ClericSkill::RadiantChannel => SkillStrings::plain(
            "hud-skill-class-cleric-radiant_channel_title",
            "hud-skill-class-cleric-radiant_channel",
        ),
    }
}

fn rogue_skill_strings(skill: RogueSkill) -> SkillStrings<'static> {
    match skill {
        RogueSkill::Lithe => SkillStrings::plain(
            "hud-skill-class-rogue-lithe_title",
            "hud-skill-class-rogue-lithe",
        ),
        RogueSkill::KeenEdge => SkillStrings::plain(
            "hud-skill-class-rogue-keen_edge_title",
            "hud-skill-class-rogue-keen_edge",
        ),
        RogueSkill::Ambush => SkillStrings::plain(
            "hud-skill-class-rogue-ambush_title",
            "hud-skill-class-rogue-ambush",
        ),
        RogueSkill::DeadlyPrecision => SkillStrings::plain(
            "hud-skill-class-rogue-deadly_precision_title",
            "hud-skill-class-rogue-deadly_precision",
        ),
        RogueSkill::FleetFooted => SkillStrings::plain(
            "hud-skill-class-rogue-fleet_footed_title",
            "hud-skill-class-rogue-fleet_footed",
        ),
        RogueSkill::SureStrike => SkillStrings::plain(
            "hud-skill-class-rogue-sure_strike_title",
            "hud-skill-class-rogue-sure_strike",
        ),
        RogueSkill::FindTheGap => SkillStrings::plain(
            "hud-skill-class-rogue-find_the_gap_title",
            "hud-skill-class-rogue-find_the_gap",
        ),
        RogueSkill::QuickHands => SkillStrings::plain(
            "hud-skill-class-rogue-quick_hands_title",
            "hud-skill-class-rogue-quick_hands",
        ),
        RogueSkill::ToxinTolerance => SkillStrings::plain(
            "hud-skill-class-rogue-toxin_tolerance_title",
            "hud-skill-class-rogue-toxin_tolerance",
        ),
        RogueSkill::Opportunist => SkillStrings::plain(
            "hud-skill-class-rogue-opportunist_title",
            "hud-skill-class-rogue-opportunist",
        ),
        RogueSkill::Shadowstep => SkillStrings::plain(
            "hud-skill-class-rogue-shadowstep_title",
            "hud-skill-class-rogue-shadowstep",
        ),
        RogueSkill::Vanish => SkillStrings::plain(
            "hud-skill-class-rogue-vanish_title",
            "hud-skill-class-rogue-vanish",
        ),
    }
}

fn unlock_skill_strings(group: SkillGroupKind) -> SkillStrings<'static> {
    match group {
        SkillGroupKind::Weapon(ToolKind::Sword) => {
            SkillStrings::plain("hud-skill-unlck_sword_title", "hud-skill-unlck_sword")
        },
        SkillGroupKind::Weapon(ToolKind::Axe) => {
            SkillStrings::plain("hud-skill-unlck_axe_title", "hud-skill-unlck_axe")
        },
        SkillGroupKind::Weapon(ToolKind::Hammer) => {
            SkillStrings::plain("hud-skill-unlck_hammer_title", "hud-skill-unlck_hammer")
        },
        SkillGroupKind::Weapon(ToolKind::Bow) => {
            SkillStrings::plain("hud-skill-unlck_bow_title", "hud-skill-unlck_bow")
        },
        SkillGroupKind::Weapon(ToolKind::Staff) => {
            SkillStrings::plain("hud-skill-unlck_staff_title", "hud-skill-unlck_staff")
        },
        SkillGroupKind::Weapon(ToolKind::Sceptre) => {
            SkillStrings::plain("hud-skill-unlck_sceptre_title", "hud-skill-unlck_sceptre")
        },
        SkillGroupKind::WeaponRoled(ToolKind::Staff, WeaponRole::Martial) => SkillStrings::plain(
            "hud-skill-unlck_staff_martial_title",
            "hud-skill-unlck_staff_martial",
        ),
        SkillGroupKind::General
        | SkillGroupKind::Class(_)
        | SkillGroupKind::Feats
        | SkillGroupKind::PactBlade
        | SkillGroupKind::Weapon(
            ToolKind::Dagger
            | ToolKind::Shield
            | ToolKind::Spear
            | ToolKind::Blowgun
            | ToolKind::Debug
            | ToolKind::Farming
            | ToolKind::Instrument
            | ToolKind::Throwable
            | ToolKind::Pick
            | ToolKind::Shovel
            | ToolKind::Natural
            | ToolKind::Empty
            | ToolKind::Tome
            | ToolKind::HolySymbol
            | ToolKind::Focus,
        )
        | SkillGroupKind::WeaponRoled(_, _) => {
            tracing::warn!("Requesting title for unlocking unexpected skill group");
            SkillStrings::Empty
        },
    }
}

fn sceptre_skill_strings(skill: SceptreSkill) -> SkillStrings<'static> {
    let modifiers = SKILL_MODIFIERS.sceptre_tree;
    match skill {
        // Lifesteal beam upgrades
        SceptreSkill::LDamage => SkillStrings::with_mult(
            "hud-skill-sc_lifesteal_damage_title",
            "hud-skill-sc_lifesteal_damage",
            modifiers.beam.damage,
        ),
        SceptreSkill::LRange => SkillStrings::with_mult(
            "hud-skill-sc_lifesteal_range_title",
            "hud-skill-sc_lifesteal_range",
            modifiers.beam.range,
        ),
        SceptreSkill::LLifesteal => SkillStrings::with_mult(
            "hud-skill-sc_lifesteal_lifesteal_title",
            "hud-skill-sc_lifesteal_lifesteal",
            modifiers.beam.lifesteal,
        ),
        SceptreSkill::LRegen => SkillStrings::with_mult(
            "hud-skill-sc_lifesteal_regen_title",
            "hud-skill-sc_lifesteal_regen",
            modifiers.beam.energy_regen,
        ),
        // Healing aura upgrades
        SceptreSkill::HHeal => SkillStrings::with_mult(
            "hud-skill-sc_heal_heal_title",
            "hud-skill-sc_heal_heal",
            modifiers.healing_aura.strength,
        ),
        SceptreSkill::HRange => SkillStrings::with_mult(
            "hud-skill-sc_heal_range_title",
            "hud-skill-sc_heal_range",
            modifiers.healing_aura.range,
        ),
        SceptreSkill::HDuration => SkillStrings::with_mult(
            "hud-skill-sc_heal_duration_title",
            "hud-skill-sc_heal_duration",
            modifiers.healing_aura.duration,
        ),
        SceptreSkill::HCost => SkillStrings::with_mult(
            "hud-skill-sc_heal_cost_title",
            "hud-skill-sc_heal_cost",
            modifiers.healing_aura.energy_cost,
        ),
        // Warding aura upgrades
        SceptreSkill::UnlockAura => SkillStrings::plain(
            "hud-skill-sc_wardaura_unlock_title",
            "hud-skill-sc_wardaura_unlock",
        ),
        SceptreSkill::AStrength => SkillStrings::with_mult(
            "hud-skill-sc_wardaura_strength_title",
            "hud-skill-sc_wardaura_strength",
            modifiers.warding_aura.strength,
        ),
        SceptreSkill::ADuration => SkillStrings::with_mult(
            "hud-skill-sc_wardaura_duration_title",
            "hud-skill-sc_wardaura_duration",
            modifiers.warding_aura.duration,
        ),
        SceptreSkill::ARange => SkillStrings::with_mult(
            "hud-skill-sc_wardaura_range_title",
            "hud-skill-sc_wardaura_range",
            modifiers.warding_aura.range,
        ),
        SceptreSkill::ACost => SkillStrings::with_mult(
            "hud-skill-sc_wardaura_cost_title",
            "hud-skill-sc_wardaura_cost",
            modifiers.warding_aura.energy_cost,
        ),
    }
}

fn climb_skill_strings(skill: ClimbSkill) -> SkillStrings<'static> {
    let modifiers = SKILL_MODIFIERS.general_tree.climb;
    match skill {
        ClimbSkill::Cost => SkillStrings::with_mult(
            "hud-skill-climbing_cost_title",
            "hud-skill-climbing_cost",
            modifiers.energy_cost,
        ),
        ClimbSkill::Speed => SkillStrings::with_mult(
            "hud-skill-climbing_speed_title",
            "hud-skill-climbing_speed",
            modifiers.speed,
        ),
    }
}

fn swim_skill_strings(skill: SwimSkill) -> SkillStrings<'static> {
    let modifiers = SKILL_MODIFIERS.general_tree.swim;
    match skill {
        SwimSkill::Speed => SkillStrings::with_mult(
            "hud-skill-swim_speed_title",
            "hud-skill-swim_speed",
            modifiers.speed,
        ),
    }
}

fn mining_skill_strings(skill: MiningSkill) -> SkillStrings<'static> {
    let modifiers = SKILL_MODIFIERS.mining_tree;
    match skill {
        MiningSkill::Speed => SkillStrings::with_mult(
            "hud-skill-pick_strike_speed_title",
            "hud-skill-pick_strike_speed",
            modifiers.speed,
        ),
        MiningSkill::OreGain => SkillStrings::with_const(
            "hud-skill-pick_strike_oregain_title",
            "hud-skill-pick_strike_oregain",
            (modifiers.ore_gain * 100.0).round() as u32,
        ),
        MiningSkill::GemGain => SkillStrings::with_const(
            "hud-skill-pick_strike_gemgain_title",
            "hud-skill-pick_strike_gemgain",
            (modifiers.gem_gain * 100.0).round() as u32,
        ),
    }
}

/// Helper object used returned by `skill_strings` as source for
/// later internationalization and formatting.
enum SkillStrings<'a> {
    Plain {
        title: &'a str,
        desc: &'a str,
    },
    WithConst {
        title: &'a str,
        desc: &'a str,
        constant: u32,
    },
    WithMult {
        title: &'a str,
        desc: &'a str,
        multiplier: f32,
    },
    Empty,
}

impl<'a> SkillStrings<'a> {
    fn plain(title: &'a str, desc: &'a str) -> Self { Self::Plain { title, desc } }

    fn with_const(title: &'a str, desc: &'a str, constant: u32) -> Self {
        Self::WithConst {
            title,
            desc,
            constant,
        }
    }

    fn with_mult(title: &'a str, desc: &'a str, multiplier: f32) -> Self {
        Self::WithMult {
            title,
            desc,
            multiplier,
        }
    }

    fn localize<'loc>(
        &self,
        i18n: &'loc Localization,
        skill_set: &SkillSet,
        skill: Skill,
    ) -> (Cow<'loc, str>, Cow<'loc, str>) {
        match self {
            Self::Plain { title, desc } => {
                let title = i18n.get_msg(title);

                let args = i18n::fluent_args! {
                    "SP" => sp(i18n, skill_set, skill),
                };
                let desc = i18n.get_msg_ctx(desc, &args);

                (title, desc)
            },
            Self::WithConst {
                title,
                desc,
                constant,
            } => {
                let title = i18n.get_msg(title);
                let args = i18n::fluent_args! {
                    "boost" => constant,
                    "SP" => sp(i18n, skill_set, skill),
                };
                let desc = i18n.get_msg_ctx(desc, &args);

                (title, desc)
            },
            Self::WithMult {
                title,
                desc,
                multiplier,
            } => {
                let percentage = hud::multiplier_to_percentage(*multiplier).abs();

                let title = i18n.get_msg(title);

                let args = i18n::fluent_args! {
                    "boost" => format!("{percentage:.0}"),
                    "SP" => sp(i18n, skill_set, skill),
                };
                let desc = i18n.get_msg_ctx(desc, &args);

                (title, desc)
            },
            Self::Empty => (Cow::Borrowed(""), Cow::Borrowed("")),
        }
    }
}

/// The number of variants of the [`CharacterStat`] enum.
const STAT_COUNT: usize = 17;

#[derive(EnumIter)]
enum CharacterStat {
    Name,
    Level,
    BattleMode,
    Waypoint,
    Hitpoints,
    Energy,
    Poise,
    CombatRating,
    Protection,
    StunResistance,
    PrecisionPower,
    EnergyReward,
    Stealth,
    WeaponPower,
    WeaponSpeed,
    WeaponEffectPower,
    /// Warlock-only: shows the bound patron and pact standing. Blank for
    /// every other class, same as `WeaponPower`/`WeaponSpeed` blank out
    /// when there's no weapon to report on.
    Pact,
}

impl CharacterStat {
    fn localized_str<'a>(&self, i18n: &'a Localization) -> Cow<'a, str> {
        use CharacterStat::*;

        match self {
            Name => i18n.get_msg("character_window-character_name"),
            Level => i18n.get_msg("character_window-character_level"),
            BattleMode => i18n.get_msg("hud-battle-mode"),
            Waypoint => i18n.get_msg("hud-waypoint"),
            Hitpoints => i18n.get_msg("hud-bag-health"),
            Energy => i18n.get_msg("hud-bag-energy"),
            CombatRating => i18n.get_msg("hud-bag-combat_rating"),
            Protection => i18n.get_msg("hud-bag-protection"),
            StunResistance => i18n.get_msg("hud-bag-stun_res"),
            Poise => i18n.get_msg("common-stats-poise_res"),
            PrecisionPower => i18n.get_msg("common-stats-precision_power"),
            EnergyReward => i18n.get_msg("common-stats-energy_reward"),
            Stealth => i18n.get_msg("common-stats-stealth"),
            WeaponPower => i18n.get_msg("common-stats-power"),
            WeaponSpeed => i18n.get_msg("common-stats-speed"),
            WeaponEffectPower => i18n.get_msg("common-stats-effect-power"),
            Pact => i18n.get_msg("hud-warlock-pact"),
        }
    }
}

fn sp<'loc>(i18n: &'loc Localization, skill_set: &SkillSet, skill: Skill) -> Cow<'loc, str> {
    let current_level = skill_set.skill_level(skill);
    if matches!(current_level, Ok(level) if level == skill.max_level()) {
        Cow::Borrowed("")
    } else {
        i18n.get_msg_ctx("hud-skill-req_sp", &i18n::fluent_args! {
            "number" => skill_set.skill_cost(skill),
        })
    }
}

#[cfg(test)]
mod tests {
    use common::comp::spell::SpellCompendium;
    use i18n::{LocalizationHandle, REFERENCE_LANG};

    /// Xindeler: every catalogued spell must have a display name in the
    /// reference language, or its row in the Diary spell tab renders as a raw
    /// i18n key. The cheap guard against a later content pass adding spells
    /// and forgetting the strings.
    #[test]
    fn every_spell_has_a_name_string() {
        let compendium = SpellCompendium::load_expect_cloned();
        assert!(!compendium.is_empty(), "the spell compendium is empty");

        let localization = LocalizationHandle::load_expect(REFERENCE_LANG);
        let localization = localization.read();

        let missing: Vec<&str> = compendium
            .iter()
            .filter(|spell| localization.try_msg(&spell.name_i18n).is_none())
            .map(|spell| spell.name_i18n.as_str())
            .collect();

        assert!(
            missing.is_empty(),
            "spells missing a name in {REFERENCE_LANG}/hud/spells.ftl: {missing:?}"
        );
    }
}
