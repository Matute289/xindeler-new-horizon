//! The creature half of the Identify inspect card (see `common::comp::
//! detection::DetectDetail::Creature`). The item half reuses `ItemTooltip`
//! verbatim (see `hud/mod.rs`'s Identify-card render block) -- this widget
//! is the one genuinely new piece of UI this phase adds.
//!
//! Every row here maps to an already-synced component (`Stats`, `SkillSet`,
//! `CharacterClass`, `Body`, `Health`/`Energy`/`Poise`, `Alignment`,
//! `Buffs`) -- Identify does not add a new sync channel, only a UI
//! permission to open this card at all (see `DetectDetail`'s own doc
//! comment). `tier` (read off the target's `DetectDetail::Creature` entry in
//! the caster's own `Detected`) gates which of those rows actually render;
//! see `server/src/sys/detection.rs`'s `IdentifyLinks` doc comment for the
//! authoritative tier→field table this mirrors.

use super::util;
use client::Client;
use common::comp::{
    Alignment, Body, Buffs, CharacterClass, Energy, Health, Poise, SkillSet, Stats,
};
use conrod_core::{
    Color, Colorable, Positionable, Widget, WidgetCommon,
    widget::{self, Text},
    widget_ids,
};
use i18n::{Localization, fluent_args};
use specs::WorldExt;

use crate::ui::fonts::Fonts;

widget_ids! {
    struct Ids {
        background,
        title,
        rows[],
        close_hint,
    }
}

#[derive(WidgetCommon)]
pub struct CreatureCard<'a> {
    #[conrod(common_builder)]
    common: widget::CommonBuilder,
    client: &'a Client,
    target: specs::Entity,
    /// See this module's own doc comment for the tier→row table.
    tier: u8,
    fonts: &'a Fonts,
    i18n: &'a Localization,
}

impl<'a> CreatureCard<'a> {
    pub fn new(
        client: &'a Client,
        target: specs::Entity,
        tier: u8,
        fonts: &'a Fonts,
        i18n: &'a Localization,
    ) -> Self {
        Self {
            common: widget::CommonBuilder::default(),
            client,
            target,
            tier,
            fonts,
            i18n,
        }
    }
}

pub struct State {
    ids: Ids,
}

const WIDTH: f64 = 300.0;
const V_PAD: f64 = 8.0;
const H_PAD: f64 = 12.0;
const ROW_H: f64 = 20.0;
const BG_COLOR: Color = Color::Rgba(0.0, 0.0, 0.0, 0.85);
const TEXT_COLOR: Color = Color::Rgba(0.9, 0.9, 0.9, 1.0);
const HINT_COLOR: Color = Color::Rgba(0.6, 0.6, 0.6, 1.0);

fn alignment_key(alignment: Alignment) -> &'static str {
    match alignment {
        Alignment::Wild => "hud-creature_card-alignment_wild",
        Alignment::Enemy => "hud-creature_card-alignment_enemy",
        Alignment::Npc => "hud-creature_card-alignment_npc",
        Alignment::Tame => "hud-creature_card-alignment_tame",
        // The owner's own name isn't resolved here -- keeping this card to
        // already-synced, already-cheap-to-read components (no extra Uid ->
        // name lookup) per this module's own doc comment.
        Alignment::Owned(_) => "hud-creature_card-alignment_owned",
        Alignment::Passive => "hud-creature_card-alignment_passive",
    }
}

impl Widget for CreatureCard<'_> {
    type Event = ();
    type State = State;
    type Style = ();

    fn init_state(&self, id_gen: widget::id::Generator) -> Self::State {
        State {
            ids: Ids::new(id_gen),
        }
    }

    fn style(&self) -> Self::Style {}

    fn update(self, args: widget::UpdateArgs<Self>) -> Self::Event {
        common_base::prof_span!("CreatureCard::update");
        let widget::UpdateArgs { state, ui, .. } = args;
        let i18n = self.i18n;

        let client_state = self.client.state();
        let ecs = client_state.ecs();

        let stats_storage = ecs.read_storage::<Stats>();
        let Some(stats) = stats_storage.get(self.target) else {
            return;
        };

        let mut rows: Vec<String> = Vec::new();

        // Tier 1: name, level, class, body.
        let title = i18n.get_content(&stats.name);
        if let Some(skill_set) = ecs.read_storage::<SkillSet>().get(self.target) {
            rows.push(
                i18n.get_msg_ctx("hud-creature_card-level", &fluent_args! {
                    "level" => u32::from(skill_set.character_level()),
                })
                .into_owned(),
            );
        }
        if let Some(class) = ecs.read_storage::<CharacterClass>().get(self.target) {
            let key = format!("char_selection-class_{}", class.primary.keyword());
            rows.push(i18n.get_msg(&key).into_owned());
        }
        if let Some(body) = ecs.read_storage::<Body>().get(self.target) {
            rows.push(i18n.get_content(&body.localize_npc()));
        }

        // Tier 2: + health, energy, poise.
        if self.tier >= 2 {
            if let Some(health) = ecs.read_storage::<Health>().get(self.target) {
                rows.push(
                    i18n.get_msg_ctx("hud-creature_card-health", &fluent_args! {
                        "current" => format!("{:.0}", health.current()),
                        "max" => format!("{:.0}", health.maximum()),
                    })
                    .into_owned(),
                );
            }
            if let Some(energy) = ecs.read_storage::<Energy>().get(self.target) {
                rows.push(
                    i18n.get_msg_ctx("hud-creature_card-energy", &fluent_args! {
                        "current" => format!("{:.0}", energy.current()),
                        "max" => format!("{:.0}", energy.maximum()),
                    })
                    .into_owned(),
                );
            }
            if let Some(poise) = ecs.read_storage::<Poise>().get(self.target) {
                rows.push(
                    i18n.get_msg_ctx("hud-creature_card-poise", &fluent_args! {
                        "current" => format!("{:.0}", poise.current()),
                        "max" => format!("{:.0}", poise.maximum()),
                    })
                    .into_owned(),
                );
            }
        }

        // Tier 3: + alignment, buffs.
        if self.tier >= 3 {
            if let Some(alignment) = ecs.read_storage::<Alignment>().get(self.target) {
                rows.push(i18n.get_msg(alignment_key(*alignment)).into_owned());
            }

            let buff_lines: Vec<String> = ecs
                .read_storage::<Buffs>()
                .get(self.target)
                .map(|buffs| {
                    buffs
                        .iter_active()
                        .flatten()
                        .map(|buff| {
                            let title = util::get_buff_title(buff.kind, i18n);
                            let desc = util::get_buff_desc(buff.kind, buff.data, i18n);
                            format!("- {title}: {desc}")
                        })
                        .collect()
                })
                .unwrap_or_default();
            if buff_lines.is_empty() {
                rows.push(i18n.get_msg("hud-creature_card-no_buffs").into_owned());
            } else {
                rows.push(i18n.get_msg("hud-creature_card-buffs").into_owned());
                rows.extend(buff_lines);
            }
        }

        // Tier 4 (full card): + resistances.
        if self.tier >= 4 {
            rows.push(
                i18n.get_msg_ctx("hud-creature_card-resistances", &fluent_args! {
                    "fire" => format!("{:.0}", stats.resist_fire * 100.0),
                    "frost" => format!("{:.0}", stats.resist_frost * 100.0),
                    "poison" => format!("{:.0}", stats.resist_poison * 100.0),
                    "magic" => format!("{:.0}", stats.resist_magic * 100.0),
                })
                .into_owned(),
            );
        }

        drop(stats_storage);

        let close_hint = i18n.get_msg("hud-creature_card-close_hint").into_owned();

        let height =
            V_PAD * 3.0 + ROW_H /* title */ + rows.len() as f64 * ROW_H + ROW_H /* hint */;

        widget::Rectangle::fill([WIDTH, height])
            .top_right_with_margins_on(ui.window, 80.0, 20.0)
            .color(BG_COLOR)
            .set(state.ids.background, ui);

        Text::new(&title)
            .top_left_with_margins_on(state.ids.background, V_PAD, H_PAD)
            .font_size(self.fonts.cyri.scale(20))
            .font_id(self.fonts.cyri.conrod_id)
            .color(conrod_core::color::WHITE)
            .set(state.ids.title, ui);

        state.update(|s| s.ids.rows.resize(rows.len(), &mut ui.widget_id_generator()));

        let mut previous = state.ids.title;
        for (i, row) in rows.iter().enumerate() {
            let text = Text::new(row)
                .down_from(previous, if i == 0 { V_PAD } else { 2.0 })
                .align_left_of(state.ids.title)
                .font_size(self.fonts.cyri.scale(14))
                .font_id(self.fonts.cyri.conrod_id)
                .color(TEXT_COLOR);
            text.set(state.ids.rows[i], ui);
            previous = state.ids.rows[i];
        }

        Text::new(&close_hint)
            .down_from(previous, V_PAD)
            .align_left_of(state.ids.title)
            .font_size(self.fonts.cyri.scale(11))
            .font_id(self.fonts.cyri.conrod_id)
            .color(HINT_COLOR)
            .set(state.ids.close_hint, ui);
    }
}
