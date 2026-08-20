use super::{TEXT_COLOR, img_ids::Imgs, settings_window::SettingsTab};
use crate::{
    ui::fonts::Fonts,
    window::{LastInput, MenuInput},
};
use conrod_core::{
    Borderable, Color, Labelable, Positionable, Sizeable, Widget, WidgetCommon, color,
    widget::{self, Button, Image},
    widget_ids,
};
use i18n::Localization;

widget_ids! {
    struct Ids {
        esc_bg,
        banner_top,
        menu_button_1,
        menu_button_2,
        menu_button_3,
        menu_button_4,
        menu_button_5,
        menu_button_6,
        menu_button_7,
    }
}

#[derive(WidgetCommon)]
pub struct EscMenu<'a> {
    imgs: &'a Imgs,
    fonts: &'a Fonts,
    localized_strings: &'a Localization,
    menu_events: &'a [MenuInput],
    last_input: LastInput,

    #[conrod(common_builder)]
    common: widget::CommonBuilder,
}

impl<'a> EscMenu<'a> {
    pub fn new(
        imgs: &'a Imgs,
        fonts: &'a Fonts,
        localized_strings: &'a Localization,
        menu_events: &'a [MenuInput],
        last_input: LastInput,
    ) -> Self {
        Self {
            imgs,
            fonts,
            localized_strings,
            menu_events,
            last_input,
            common: widget::CommonBuilder::default(),
        }
    }
}

pub struct State {
    ids: Ids,
    // Gamepad/keyboard menu navigation: index into the 7 buttons in visual
    // (top-to-bottom) order — Resume, Settings, Controls, Characters, Report Bug,
    // Logout, Quit. Mirrors the `ContextMenu` list-nav shape in `slot_grid.rs`.
    active_button: usize,
}

const BUTTON_COUNT: usize = 7;

pub enum Event {
    OpenSettings(SettingsTab),
    CharacterSelection,
    ReportBug,
    Logout,
    Quit,
    Close,
}

impl Widget for EscMenu<'_> {
    type Event = Option<Event>;
    type State = State;
    type Style = ();

    fn init_state(&self, id_gen: widget::id::Generator) -> Self::State {
        State {
            ids: Ids::new(id_gen),
            active_button: 0,
        }
    }

    fn style(&self) -> Self::Style {}

    fn update(self, args: widget::UpdateArgs<Self>) -> Self::Event {
        common_base::prof_span!("EscMenu::update");
        let widget::UpdateArgs { state, ui, .. } = args;

        // MENU INPUTS: `Back` closes the esc menu (same as "Resume"); Up/Down moves
        // the highlighted button (no wrap); Apply activates it — the `ContextMenu`
        // list-nav shape from `slot_grid.rs`, replicated locally since that struct
        // isn't exported.
        let mut apply_pressed = false;
        for key in self.menu_events {
            match key {
                MenuInput::Back => return Some(Event::Close),
                MenuInput::Up => state.update(|s| {
                    s.active_button = s.active_button.saturating_sub(1);
                }),
                MenuInput::Down => state.update(|s| {
                    s.active_button = (s.active_button + 1).min(BUTTON_COUNT - 1);
                }),
                MenuInput::Apply => apply_pressed = true,
                _ => {},
            }
        }
        let menu_active = matches!(self.last_input, LastInput::Keyboard | LastInput::Controller);
        let is_highlighted = |index: usize| menu_active && state.active_button == index;

        Image::new(self.imgs.esc_frame)
            .w_h(240.0, 440.0)
            .color(Some(Color::Rgba(1.0, 1.0, 1.0, 0.9)))
            .middle_of(ui.window)
            .set(state.ids.esc_bg, ui);

        Image::new(self.imgs.banner_top)
            .w_h(250.0, 34.0)
            .mid_top_with_margin_on(state.ids.esc_bg, -34.0)
            .set(state.ids.banner_top, ui);

        // Resume
        if Button::image(self.imgs.button)
            .mid_bottom_with_margin_on(state.ids.banner_top, -60.0)
            .w_h(210.0, 50.0)
            .hover_image(self.imgs.button_hover)
            .press_image(self.imgs.button_press)
            .label(&self.localized_strings.get_msg("common-resume"))
            .label_y(conrod_core::position::Relative::Scalar(3.0))
            .label_color(TEXT_COLOR)
            .label_font_size(self.fonts.cyri.scale(20))
            .label_font_id(self.fonts.cyri.conrod_id)
            .border(if is_highlighted(0) { 2.0 } else { 0.0 })
            .border_color(if is_highlighted(0) {
                color::YELLOW
            } else {
                color::TRANSPARENT
            })
            .set(state.ids.menu_button_1, ui)
            .was_clicked()
            || (apply_pressed && is_highlighted(0))
        {
            return Some(Event::Close);
        };

        // Settings
        if Button::image(self.imgs.button)
            .mid_bottom_with_margin_on(state.ids.menu_button_1, -65.0)
            .w_h(210.0, 50.0)
            .hover_image(self.imgs.button_hover)
            .press_image(self.imgs.button_press)
            .label(&self.localized_strings.get_msg("common-settings"))
            .label_y(conrod_core::position::Relative::Scalar(3.0))
            .label_color(TEXT_COLOR)
            .label_font_size(self.fonts.cyri.scale(20))
            .label_font_id(self.fonts.cyri.conrod_id)
            .border(if is_highlighted(1) { 2.0 } else { 0.0 })
            .border_color(if is_highlighted(1) {
                color::YELLOW
            } else {
                color::TRANSPARENT
            })
            .set(state.ids.menu_button_2, ui)
            .was_clicked()
            || (apply_pressed && is_highlighted(1))
        {
            return Some(Event::OpenSettings(SettingsTab::Interface));
        };
        // Controls
        if Button::image(self.imgs.button)
            .mid_bottom_with_margin_on(state.ids.menu_button_2, -55.0)
            .w_h(210.0, 50.0)
            .hover_image(self.imgs.button_hover)
            .press_image(self.imgs.button_press)
            .label(&self.localized_strings.get_msg("common-controls"))
            .label_y(conrod_core::position::Relative::Scalar(3.0))
            .label_color(TEXT_COLOR)
            .label_font_size(self.fonts.cyri.scale(20))
            .label_font_id(self.fonts.cyri.conrod_id)
            .border(if is_highlighted(2) { 2.0 } else { 0.0 })
            .border_color(if is_highlighted(2) {
                color::YELLOW
            } else {
                color::TRANSPARENT
            })
            .set(state.ids.menu_button_3, ui)
            .was_clicked()
            || (apply_pressed && is_highlighted(2))
        {
            return Some(Event::OpenSettings(SettingsTab::Controls));
        };
        // Characters
        if Button::image(self.imgs.button)
            .mid_bottom_with_margin_on(state.ids.menu_button_3, -55.0)
            .w_h(210.0, 50.0)
            .hover_image(self.imgs.button_hover)
            .press_image(self.imgs.button_press)
            .label(&self.localized_strings.get_msg("common-characters"))
            .label_y(conrod_core::position::Relative::Scalar(3.0))
            .label_color(TEXT_COLOR)
            .label_font_size(self.fonts.cyri.scale(20))
            .label_font_id(self.fonts.cyri.conrod_id)
            .border(if is_highlighted(3) { 2.0 } else { 0.0 })
            .border_color(if is_highlighted(3) {
                color::YELLOW
            } else {
                color::TRANSPARENT
            })
            .set(state.ids.menu_button_4, ui)
            .was_clicked()
            || (apply_pressed && is_highlighted(3))
        {
            return Some(Event::CharacterSelection);
        };
        // Report Bug
        if Button::image(self.imgs.button)
            .mid_bottom_with_margin_on(state.ids.menu_button_4, -65.0)
            .w_h(210.0, 50.0)
            .hover_image(self.imgs.button_hover)
            .press_image(self.imgs.button_press)
            .label("Report Bug")
            .label_y(conrod_core::position::Relative::Scalar(3.0))
            .label_color(TEXT_COLOR)
            .label_font_size(self.fonts.cyri.scale(20))
            .label_font_id(self.fonts.cyri.conrod_id)
            .border(if is_highlighted(4) { 2.0 } else { 0.0 })
            .border_color(if is_highlighted(4) {
                color::YELLOW
            } else {
                color::TRANSPARENT
            })
            .set(state.ids.menu_button_7, ui)
            .was_clicked()
            || (apply_pressed && is_highlighted(4))
        {
            return Some(Event::ReportBug);
        };
        // Logout
        if Button::image(self.imgs.button)
            .mid_bottom_with_margin_on(state.ids.menu_button_7, -55.0)
            .w_h(210.0, 50.0)
            .hover_image(self.imgs.button_hover)
            .press_image(self.imgs.button_press)
            .label(&self.localized_strings.get_msg("esc_menu-logout"))
            .label_y(conrod_core::position::Relative::Scalar(3.0))
            .label_color(TEXT_COLOR)
            .label_font_size(self.fonts.cyri.scale(20))
            .label_font_id(self.fonts.cyri.conrod_id)
            .border(if is_highlighted(5) { 2.0 } else { 0.0 })
            .border_color(if is_highlighted(5) {
                color::YELLOW
            } else {
                color::TRANSPARENT
            })
            .set(state.ids.menu_button_5, ui)
            .was_clicked()
            || (apply_pressed && is_highlighted(5))
        {
            return Some(Event::Logout);
        };
        // Quit
        if Button::image(self.imgs.button)
            .mid_bottom_with_margin_on(state.ids.menu_button_5, -55.0)
            .w_h(210.0, 50.0)
            .hover_image(self.imgs.button_hover)
            .press_image(self.imgs.button_press)
            .label(&self.localized_strings.get_msg("esc_menu-quit_game"))
            .label_y(conrod_core::position::Relative::Scalar(3.0))
            .label_color(TEXT_COLOR)
            .label_font_size(self.fonts.cyri.scale(20))
            .label_font_id(self.fonts.cyri.conrod_id)
            .border(if is_highlighted(6) { 2.0 } else { 0.0 })
            .border_color(if is_highlighted(6) {
                color::YELLOW
            } else {
                color::TRANSPARENT
            })
            .set(state.ids.menu_button_6, ui)
            .was_clicked()
            || (apply_pressed && is_highlighted(6))
        {
            return Some(Event::Quit);
        };
        None
    }
}
