//! Chat on-screen keyboard (OSK) — a text composer for players with no
//! physical keyboard attached, opened from `Hud::handle_event` when
//! `GameInput::Chat`/`Command` fires while the last input device was a
//! controller.
//!
//! This widget owns its own text buffer and, on submit, feeds the composed
//! string through the same command-prefix parsing (`chat::parse_cmd`) and
//! `Event::SendMessage`/`Event::SendCommand` dispatch the real chat input
//! uses, rather than trying to synthesize a keypress into `chat.rs`'s
//! `TextEdit` (conrod has no hook for injecting a synthetic keypress into
//! another widget). It does not participate in the real chat input's
//! history/tab-completion — an accepted limitation of this first pass.
//!
//! Navigation follows the list-nav shape used elsewhere in this module
//! (`slot_grid.rs`, `esc_menu.rs`): dpad `Up`/`Down`/`Left`/`Right` moves a
//! highlighted cell over a jagged character grid, `Apply` activates the
//! highlighted cell (types a character, or performs the Space/Backspace/
//! Submit special cell), `Back` closes. Space/Backspace/Submit are ordinary
//! grid cells selected the same way as every letter, rather than extra
//! button bindings — `North`/`Select` already have unrelated default
//! `MenuInput` meanings (`LocalFocus`/`GlobalFocus`), so reusing the
//! existing `Up`/`Down`/`Left`/`Right`/`Apply`/`Back` vocabulary end-to-end
//! keeps this consistent with every other window's input handling instead
//! of inventing a gamepad-only parallel path.

use super::{TEXT_COLOR, img_ids::Imgs};
use crate::{
    ui::fonts::Fonts,
    window::{LastInput, MenuInput},
};
use conrod_core::{
    Borderable, Colorable, Labelable, Positionable, Sizeable, Widget, WidgetCommon, color,
    widget::{self, Button, Rectangle, Text},
    widget_ids,
};
use i18n::Localization;

const KEY_SIZE: f64 = 32.0;
const KEY_SPACING: f64 = 4.0;
const PANEL_MARGIN: f64 = 16.0;
// Vertical space reserved above the character grid for the title and the
// live buffer-preview line.
const GRID_TOP: f64 = 70.0;

#[derive(Clone, Copy, PartialEq, Eq)]
enum OskKey {
    Char(char),
    Space,
    Backspace,
    Submit,
}

impl OskKey {
    fn label(self) -> String {
        match self {
            OskKey::Char(c) => c.to_string(),
            OskKey::Space => "SPACE".to_string(),
            OskKey::Backspace => "<-".to_string(),
            OskKey::Submit => "OK".to_string(),
        }
    }
}

use OskKey::{Backspace, Char, Space, Submit};

/// Jagged character grid. Rows 0-3 are a QWERTY-ish layout (lowercase only —
/// no shift/caps in this first pass), row 4 covers common chat punctuation
/// (including `/` for slash-commands), row 5 holds the three special cells.
const ROWS: &[&[OskKey]] = &[
    &[
        Char('1'),
        Char('2'),
        Char('3'),
        Char('4'),
        Char('5'),
        Char('6'),
        Char('7'),
        Char('8'),
        Char('9'),
        Char('0'),
    ],
    &[
        Char('q'),
        Char('w'),
        Char('e'),
        Char('r'),
        Char('t'),
        Char('y'),
        Char('u'),
        Char('i'),
        Char('o'),
        Char('p'),
    ],
    &[
        Char('a'),
        Char('s'),
        Char('d'),
        Char('f'),
        Char('g'),
        Char('h'),
        Char('j'),
        Char('k'),
        Char('l'),
    ],
    &[
        Char('z'),
        Char('x'),
        Char('c'),
        Char('v'),
        Char('b'),
        Char('n'),
        Char('m'),
    ],
    &[
        Char('.'),
        Char(','),
        Char('!'),
        Char('?'),
        Char('/'),
        Char('-'),
        Char('\''),
    ],
    &[Space, Backspace, Submit],
];

fn total_keys() -> usize { ROWS.iter().map(|row| row.len()).sum() }

widget_ids! {
    struct Ids {
        backdrop,
        title,
        buffer_text,
        keys[],
    }
}

#[derive(WidgetCommon)]
pub struct Osk<'a> {
    #[conrod(common_builder)]
    common: widget::CommonBuilder,
    imgs: &'a Imgs,
    fonts: &'a Fonts,
    localized_strings: &'a Localization,
    menu_events: &'a [MenuInput],
    last_input: LastInput,
    prefill: Option<String>,
}

impl<'a> Osk<'a> {
    pub fn new(
        imgs: &'a Imgs,
        fonts: &'a Fonts,
        localized_strings: &'a Localization,
        menu_events: &'a [MenuInput],
        last_input: LastInput,
    ) -> Self {
        Self {
            common: widget::CommonBuilder::default(),
            imgs,
            fonts,
            localized_strings,
            menu_events,
            last_input,
            prefill: None,
        }
    }

    /// Seeds the buffer once (e.g. `/` when opened via `GameInput::Command`).
    /// Callers should only pass `Some` on the frame the OSK opens — see
    /// `Hud::osk_prefill.take()`.
    pub fn prefill(mut self, text: String) -> Self {
        self.prefill = Some(text);
        self
    }
}

pub struct State {
    ids: Ids,
    buffer: String,
    // (row, col) into `ROWS` — jagged, so `col` is re-clamped on every row
    // change (mirrors `SlotGrid`'s `[x, y]` handling of non-rectangular
    // grids).
    active: (usize, usize),
}

pub enum Event {
    /// Composed message, ready for the same command-prefix check the real
    /// chat input performs.
    Submit(String),
    Close,
}

impl Widget for Osk<'_> {
    type Event = Option<Event>;
    type State = State;
    type Style = ();

    fn init_state(&self, id_gen: widget::id::Generator) -> Self::State {
        State {
            ids: Ids::new(id_gen),
            buffer: String::new(),
            active: (0, 0),
        }
    }

    fn style(&self) -> Self::Style {}

    fn update(self, args: widget::UpdateArgs<Self>) -> Self::Event {
        common_base::prof_span!("Osk::update");
        let widget::UpdateArgs { state, ui, .. } = args;

        if let Some(text) = self.prefill {
            state.update(|s| s.buffer = text);
        }

        let mut submitted: Option<String> = None;
        for key in self.menu_events {
            match key {
                MenuInput::Up => state.update(|s| {
                    let (row, col) = s.active;
                    if row > 0 {
                        let new_row = row - 1;
                        let clamped_col = col.min(ROWS[new_row].len() - 1);
                        s.active = (new_row, clamped_col);
                    }
                }),
                MenuInput::Down => state.update(|s| {
                    let (row, col) = s.active;
                    if row + 1 < ROWS.len() {
                        let new_row = row + 1;
                        let clamped_col = col.min(ROWS[new_row].len() - 1);
                        s.active = (new_row, clamped_col);
                    }
                }),
                MenuInput::Left => state.update(|s| {
                    let (row, col) = s.active;
                    if col > 0 {
                        s.active = (row, col - 1);
                    }
                }),
                MenuInput::Right => state.update(|s| {
                    let (row, col) = s.active;
                    if col + 1 < ROWS[row].len() {
                        s.active = (row, col + 1);
                    }
                }),
                MenuInput::Apply => {
                    let (row, col) = state.active;
                    match ROWS[row][col] {
                        OskKey::Char(c) => state.update(|s| s.buffer.push(c)),
                        OskKey::Space => state.update(|s| s.buffer.push(' ')),
                        OskKey::Backspace => state.update(|s| {
                            s.buffer.pop();
                        }),
                        OskKey::Submit => submitted = Some(state.buffer.clone()),
                    }
                },
                MenuInput::Back => return Some(Event::Close),
                _ => {},
            }
        }
        if let Some(msg) = submitted {
            return Some(Event::Submit(msg));
        }

        let cols = ROWS.iter().map(|row| row.len()).max().unwrap_or(1);
        let panel_w = (cols as f64) * (KEY_SIZE + KEY_SPACING) + PANEL_MARGIN * 2.0;
        let panel_h = GRID_TOP + (ROWS.len() as f64) * (KEY_SIZE + KEY_SPACING) + PANEL_MARGIN;

        Rectangle::fill([panel_w, panel_h])
            .rgba(0.0, 0.0, 0.0, 0.9)
            .middle_of(ui.window)
            .set(state.ids.backdrop, ui);

        Text::new(&self.localized_strings.get_msg("hud-osk-title"))
            .top_left_with_margins_on(state.ids.backdrop, PANEL_MARGIN, PANEL_MARGIN)
            .font_size(self.fonts.cyri.scale(16))
            .font_id(self.fonts.cyri.conrod_id)
            .color(TEXT_COLOR)
            .set(state.ids.title, ui);

        Text::new(&format!("{}_", state.buffer))
            .down_from(state.ids.title, 8.0)
            .w(panel_w - PANEL_MARGIN * 2.0)
            .font_size(self.fonts.universal.scale(16))
            .font_id(self.fonts.universal.conrod_id)
            .color(TEXT_COLOR)
            .set(state.ids.buffer_text, ui);

        if state.ids.keys.len() < total_keys() {
            state.update(|s| {
                s.ids
                    .keys
                    .resize(total_keys(), &mut ui.widget_id_generator());
            });
        }

        let menu_active = matches!(self.last_input, LastInput::Keyboard | LastInput::Controller);

        // Explicit pixel placement relative to the backdrop (rather than a
        // chain of `down_from`/`right_from` calls) mirrors `SlotGrid`'s
        // established approach for laying out a grid of same-size cells.
        let mut key_index = 0;
        for (row_i, row) in ROWS.iter().enumerate() {
            for (col_i, key) in row.iter().enumerate() {
                let id = state.ids.keys[key_index];
                key_index += 1;

                let highlighted = menu_active && state.active == (row_i, col_i);
                let x_pos = PANEL_MARGIN + (col_i as f64) * (KEY_SIZE + KEY_SPACING);
                let y_pos = GRID_TOP + (row_i as f64) * (KEY_SIZE + KEY_SPACING);

                Button::image(if highlighted {
                    self.imgs.selection
                } else {
                    self.imgs.nothing
                })
                .label(&key.label())
                .label_color(TEXT_COLOR)
                .label_font_size(self.fonts.cyri.scale(12))
                .label_font_id(self.fonts.cyri.conrod_id)
                .w_h(KEY_SIZE, KEY_SIZE)
                .border(if highlighted { 2.0 } else { 0.5 })
                .border_color(if highlighted {
                    color::YELLOW
                } else {
                    color::rgba(1.0, 1.0, 1.0, 0.3)
                })
                .top_left_with_margins_on(state.ids.backdrop, y_pos, x_pos)
                .set(id, ui);
            }
        }

        None
    }
}
