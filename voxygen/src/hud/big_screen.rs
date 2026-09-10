use crate::ui::fonts::Fonts;
use common::comp;
use conrod_core::{
    Color, Colorable, Positionable, Sizeable, Widget, WidgetCommon,
    widget::{self, Rectangle, Text},
    widget_ids,
};
use i18n::Localization;
use std::{collections::VecDeque, time::Instant};

widget_ids! {
    struct Ids {
        dim_bg,
        text_bg,
        text,
    }
}

/// A mandatory-read, self-timed overlay for operator "Big Screen" messages
/// (ZG-80). Unlike [`super::popup::Popup`], it covers the whole screen and
/// carries no dismiss action -- it just renders above every other HUD/menu
/// layer for a duration computed from the message length, then clears
/// itself. The same message also always reaches the normal chat window,
/// handled separately by `Hud::new_message`; this widget only owns the
/// overlay rendering.
#[derive(WidgetCommon)]
pub struct BigScreen<'a> {
    new_messages: &'a VecDeque<comp::ChatMsg>,
    i18n: &'a Localization,
    fonts: &'a Fonts,
    #[conrod(common_builder)]
    common: widget::CommonBuilder,
}

impl<'a> BigScreen<'a> {
    pub fn new(
        new_messages: &'a VecDeque<comp::ChatMsg>,
        i18n: &'a Localization,
        fonts: &'a Fonts,
    ) -> Self {
        Self {
            new_messages,
            i18n,
            fonts,
            common: widget::CommonBuilder::default(),
        }
    }
}

pub struct State {
    ids: Ids,
    queue: VecDeque<String>,
    current: Option<(String, Instant, f32)>,
}

// Reading-speed-based duration: a fixed floor (so even a one-word message is
// readable) plus a per-character allowance, clamped to a sane range so
// neither a tiny nor a huge message produces a silly duration.
const BASE_SECS: f32 = 2.0;
const SECS_PER_CHAR: f32 = 0.06;
const MIN_SECS: f32 = 3.0;
const MAX_SECS: f32 = 15.0;
const FADE_IN: f32 = 0.4;
const FADE_OUT: f32 = 0.6;

fn duration_for(text: &str) -> f32 {
    (BASE_SECS + text.chars().count() as f32 * SECS_PER_CHAR).clamp(MIN_SECS, MAX_SECS)
}

impl Widget for BigScreen<'_> {
    type Event = ();
    type State = State;
    type Style = ();

    fn init_state(&self, id_gen: widget::id::Generator) -> Self::State {
        State {
            ids: Ids::new(id_gen),
            queue: VecDeque::new(),
            current: None,
        }
    }

    fn style(&self) -> Self::Style {}

    fn update(self, args: widget::UpdateArgs<Self>) -> Self::Event {
        common_base::prof_span!("BigScreen::update");
        let widget::UpdateArgs { state, ui, .. } = args;

        for msg in self.new_messages {
            let text = self.i18n.get_content(msg.content());
            state.update(|s| s.queue.push_back(text));
        }

        if state.current.is_none()
            && let Some(next) = state.queue.front().cloned()
        {
            state.update(|s| {
                s.queue.pop_front();
                let duration = duration_for(&next);
                s.current = Some((next, Instant::now(), duration));
            });
        }

        let Some((text, started_at, duration)) = state.current.clone() else {
            return;
        };

        let elapsed = started_at.elapsed().as_secs_f32();
        if elapsed >= duration + FADE_OUT {
            state.update(|s| s.current = None);
            return;
        }

        let fade = if elapsed < FADE_IN {
            elapsed / FADE_IN
        } else if elapsed < duration {
            1.0
        } else {
            (1.0 - (elapsed - duration) / FADE_OUT).max(0.0)
        };

        Rectangle::fill([0.0, 0.0])
            .wh_of(ui.window)
            .middle_of(ui.window)
            .graphics_for(ui.window)
            .color(Color::Rgba(0.0, 0.0, 0.0, 0.75 * fade))
            .set(state.ids.dim_bg, ui);
        Text::new(&text)
            .middle_of(state.ids.dim_bg)
            .w(ui.win_w * 0.7)
            .font_size(self.fonts.alkhemi.scale(45))
            .font_id(self.fonts.alkhemi.conrod_id)
            .color(Color::Rgba(0.0, 0.0, 0.0, fade))
            .set(state.ids.text_bg, ui);
        Text::new(&text)
            .top_left_with_margins_on(state.ids.text_bg, -2.0, -2.0)
            .w(ui.win_w * 0.7)
            .font_size(self.fonts.alkhemi.scale(45))
            .font_id(self.fonts.alkhemi.conrod_id)
            .color(Color::Rgba(1.0, 1.0, 1.0, fade))
            .set(state.ids.text, ui);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_short_message_floors_at_min_secs() {
        assert_eq!(duration_for("hi"), MIN_SECS);
    }

    #[test]
    fn a_very_long_message_caps_at_max_secs() {
        assert_eq!(duration_for(&"a".repeat(1000)), MAX_SECS);
    }

    #[test]
    fn a_mid_length_message_scales_with_its_character_count() {
        let text = "a".repeat(50);
        assert!((duration_for(&text) - 5.0).abs() < 0.01);
    }
}
