use crate::{
    GlobalState,
    hud::default_water_color,
    render::UiDrawer,
    ui::{
        self, Graphic, GraphicId,
        fonts::IcedFonts as Fonts,
        ice::{
            Element, IcedRenderer, IcedUi as Ui,
            component::{
                neat_button,
                tooltip::{self, WithTooltip},
            },
            style,
            widget::{
                AspectRatioContainer, BackgroundContainer, Image, MouseDetector, Overlay, Padding,
                TooltipManager, mouse_detector,
            },
        },
        img_ids::ImageGraphic,
    },
    window,
};
use client::{Client, ServerInfo};
use common::{
    LoadoutBuilder,
    character::{CharacterId, CharacterItem, MAX_CHARACTERS_PER_PLAYER, MAX_NAME_LENGTH},
    comp::{
        self, Background, BackgroundKind, Inventory, Item,
        class::ClassKind,
        ethos::{Ethos, Moral, Order},
        humanoid,
        inventory::slot::EquipSlot,
    },
    map::Marker,
    resources::Time,
    terrain::TerrainChunkSize,
    vol::RectVolSize,
};
use common_net::msg::world_msg::SiteId;
use i18n::{Localization, LocalizationHandle};
use rand::{RngExt, rng};
//ImageFrame, Tooltip,
use crate::settings::Settings;
//use std::time::Duration;
//use ui::ice::widget;
use iced::{
    Align, Button, Checkbox, Color, Column, Container, HorizontalAlignment, Length, Row,
    Scrollable, Slider, Space, Text, TextInput, VerticalAlignment, button, scrollable, slider,
    text_input,
};
use std::sync::Arc;
use vek::{Rgba, Vec2};

pub const TEXT_COLOR: iced::Color = iced::Color::from_rgb(1.0, 1.0, 1.0);
pub const DISABLED_TEXT_COLOR: iced::Color = iced::Color::from_rgba(1.0, 1.0, 1.0, 0.2);
pub const TOOLTIP_BACK_COLOR: Rgba<u8> = Rgba::new(20, 18, 10, 255);
const FILL_FRAC_ONE: f32 = 0.77;
const FILL_FRAC_TWO: f32 = 0.53;
const TOOLTIP_HOVER_DUR: std::time::Duration = std::time::Duration::from_millis(150);
const TOOLTIP_FADE_DUR: std::time::Duration = std::time::Duration::from_millis(350);
const BANNER_ALPHA: u8 = 210;
// Buttons in the bottom corners
const SMALL_BUTTON_HEIGHT: u16 = 31;

const STARTER_HAMMER: &str = "common.items.weapons.hammer.starter_hammer";
const STARTER_BOW: &str = "common.items.weapons.bow.starter";
const STARTER_AXE: &str = "common.items.weapons.axe.starter_axe";
const STARTER_STAFF: &str = "common.items.weapons.staff.starter_staff";
const STARTER_SWORD: &str = "common.items.weapons.sword.starter";
const STARTER_SWORDS: &str = "common.items.weapons.sword_1h.starter";
const STARTER_SCEPTRE: &str = "common.items.weapons.sceptre.starter_sceptre";

/// Default starter weapon shown when a class is picked; must be a member of
/// the server-side whitelist in server/src/character_creator.rs.
fn default_starter_for_class(class: ClassKind) -> (Option<&'static str>, Option<&'static str>) {
    match class {
        // Adventurer can't be picked at creation; no starter weapons (matches
        // the server's empty whitelist).
        ClassKind::Adventurer => (None, None),
        ClassKind::Warrior => (Some(STARTER_SWORD), None),
        ClassKind::Mage => (Some(STARTER_STAFF), None),
        ClassKind::Cleric => (Some(STARTER_SCEPTRE), None),
        ClassKind::Rogue => (Some(STARTER_SWORDS), Some(STARTER_SWORDS)),
        // Classes-wave (BL-04): the default mirrors the first server whitelist entry.
        ClassKind::Barbarian => (Some(STARTER_AXE), None),
        ClassKind::Sorcerer
        | ClassKind::Warlock
        | ClassKind::Bard
        | ClassKind::Druid
        | ClassKind::Artificer => (Some(STARTER_STAFF), None),
        ClassKind::Paladin | ClassKind::BloodSlayer => (Some(STARTER_SWORD), None),
        ClassKind::Ranger => (Some(STARTER_BOW), None),
        ClassKind::Monk => (Some(STARTER_SWORDS), None),
    }
}

/// Like [`neat_button`], but with a FIXED-size centred label instead of the
/// auto-scaling `FillText`, and the button simply **fills its cell** (`width:
/// Fill`) instead of taking an image-aspect-ratio width. Used by the BL-33
/// alignment picker so a row of 3 buttons splits the width into equal thirds —
/// every word renders at the same size, all buttons are the same width, and
/// none overflow the panel. Selection is shown by gold text.
fn fixed_label_button(
    state: &mut button::State,
    label: String,
    text_size: u16,
    selected: bool,
    button_style: style::button::Style,
    message: Message,
) -> Element<'_, Message> {
    let color = if selected {
        Color::from_rgb(0.93, 0.78, 0.28)
    } else {
        TEXT_COLOR
    };
    let text = Text::new(label)
        .size(text_size)
        .width(Length::Fill)
        .height(Length::Fill)
        .horizontal_alignment(HorizontalAlignment::Center)
        .vertical_alignment(VerticalAlignment::Center)
        .color(color);
    Button::new(state, text)
        .height(Length::Fill)
        .width(Length::Fill)
        .style(button_style)
        .on_press(message)
        .into()
}

/// BL-31 UI-fixes (spec §4.4/§4.5), extended by BL-31 V2 (spec §3.1/§3.3):
/// the mechanical stat passives granted by each background, transcribed
/// from `docs/design/plans/2026-07-01-backgrounds-p0-triage.md`'s "V1 Stat
/// Passive" column (first passive) plus the V2 design spec's finalized
/// second-passive table (§3.3). A plain `match` is the P4-smoke-speed
/// sourcing option the spec calls out (§4.5b); keyed by `BackgroundKind` so
/// a later variant trim needs no change here (spec §0.1). Every background
/// now shows two `; `-joined passives — Miner already had two in V1 and is
/// unchanged. Display-only: there is no apply path wired for these stat
/// strings anywhere in `server::character_creator` (only the starter-kit
/// stub `apply_background_kit` exists, and it ignores `Background`
/// entirely), so this is purely a text change (BL-31 V2 spec §3.1/plan
/// step 2 apply-path check). (Rewarded/Ruined, which used to show
/// feat-point grant text here, were cut from the catalogue by the
/// 2026-07-02 curation pass — see
/// docs/design/specs/2026-07-02-backgrounds-curation-design.md §3.)
fn background_stat_passive(kind: BackgroundKind) -> &'static str {
    match kind {
        BackgroundKind::Acolyte => "HealingReceivedMod +8%; HealingOutputMod +6%",
        BackgroundKind::Hermit => "OutOfCombatHealthRegen +12%; SpellDamageMod +3%",
        BackgroundKind::Inquisitor => "Undead/FiendDamageMod +8%; InitiativeBonus +3",
        BackgroundKind::Sage => "SpellDamageMod +4%; CritChanceMod +1%",
        BackgroundKind::Archaeologist => "MaxHealth +10; DarkvisionRange +8m",
        BackgroundKind::Scribe => "MaxHealth +6; SpellDamageMod +3%",
        BackgroundKind::Investigator => "InitiativeBonus +3; CritChanceMod +1%",
        BackgroundKind::Soldier => "MeleeDamageMod +5%; PhysicalDamageReduction +2",
        BackgroundKind::Guard => "PhysicalDamageReduction +2; InitiativeBonus +3",
        BackgroundKind::Criminal => "MoveSpeed +0.8; CritChanceMod +1%",
        BackgroundKind::Charlatan => "MaxHealth +8; InitiativeBonus +2",
        BackgroundKind::BountyHunter => "InitiativeBonus +4; MeleeDamageMod +3%",
        BackgroundKind::Noble => "MaxHealth +10; HealingReceivedMod +4%",
        BackgroundKind::Entertainer => "HealingReceivedMod +8%; MoveSpeed +0.6",
        BackgroundKind::FolkHero => "MaxHealth +10; MeleeDamageMod +3%",
        BackgroundKind::Merchant => "MoveSpeed +0.8; CritChanceMod +1%",
        BackgroundKind::Artisan => "PhysicalDamageReduction +3; MeleeDamageMod +3%",
        BackgroundKind::Farmer => "MaxHealth +12; PhysicalDamageReduction +2",
        BackgroundKind::Fisher => "ElementalResistance(cold) +10%; OutOfCombatHealthRegen +8%",
        BackgroundKind::Miner => "MaxHealth +10; DarkvisionRange +10m",
        BackgroundKind::Outlander => "OutOfCombatHealthRegen +15%; MoveSpeed +0.6",
        BackgroundKind::Guide => "MoveSpeed +1.0; InitiativeBonus +2",
        BackgroundKind::Sailor => "ElementalResistance(cold) +12%; OutOfCombatHealthRegen +8%",
        BackgroundKind::Urchin => "MoveSpeed +1.2; CritChanceMod +1%",
    }
}

/// BL-31 V2 (spec §3.1/§3.3): display-only "tipo de sociedad" flavor label
/// per background. This is **not** consumed by any NPC/reputation/
/// disposition system in V1 — it is pure flavor text shown in the
/// Habilidades section (spec §3.2). The system that would eventually read
/// this field to affect NPC disposition/pricing is deferred as BL-79 (spec
/// §7); no such system exists yet.
fn background_society_type(kind: BackgroundKind) -> &'static str {
    match kind {
        BackgroundKind::Acolyte => "Religiosa",
        BackgroundKind::Hermit => "Contemplativa",
        BackgroundKind::Inquisitor => "Religiosa",
        BackgroundKind::Sage => "Erudita",
        BackgroundKind::Archaeologist => "Erudita/Exploradora",
        BackgroundKind::Scribe => "Erudita",
        BackgroundKind::Investigator => "Erudita/Legal",
        BackgroundKind::Soldier => "Militar",
        BackgroundKind::Guard => "Militar/Urbana",
        BackgroundKind::Criminal => "Bajo mundo",
        BackgroundKind::Charlatan => "Bajo mundo/Comercial",
        BackgroundKind::BountyHunter => "Bajo mundo/Cazadores",
        BackgroundKind::Noble => "Aristocracia",
        BackgroundKind::Entertainer => "Popular/Artística",
        BackgroundKind::FolkHero => "Popular/Rural",
        BackgroundKind::Merchant => "Comercial",
        BackgroundKind::Artisan => "Gremial",
        BackgroundKind::Farmer => "Rural",
        BackgroundKind::Fisher => "Marítima",
        BackgroundKind::Miner => "Gremial/Subterránea",
        BackgroundKind::Outlander => "Salvaje/Nómada",
        BackgroundKind::Guide => "Salvaje/Nómada",
        BackgroundKind::Sailor => "Marítima",
        BackgroundKind::Urchin => "Calle/Bajo mundo urbano",
    }
}

/// BL-31 V2 (spec §4/§5): i18n key for each background's "Detalle" narrative
/// paragraph, authored verbatim in `docs/design/lore/chargen/
/// 21-background-detalle.md` and transcribed into
/// `assets/voxygen/i18n/en/char_selection.ftl` as
/// `char_selection-background_detalle_<keyword>`.
fn background_detalle(kind: BackgroundKind) -> &'static str {
    match kind {
        BackgroundKind::Acolyte => "char_selection-background_detalle_acolyte",
        BackgroundKind::Hermit => "char_selection-background_detalle_hermit",
        BackgroundKind::Inquisitor => "char_selection-background_detalle_inquisitor",
        BackgroundKind::Sage => "char_selection-background_detalle_sage",
        BackgroundKind::Archaeologist => "char_selection-background_detalle_archaeologist",
        BackgroundKind::Scribe => "char_selection-background_detalle_scribe",
        BackgroundKind::Investigator => "char_selection-background_detalle_investigator",
        BackgroundKind::Soldier => "char_selection-background_detalle_soldier",
        BackgroundKind::Guard => "char_selection-background_detalle_guard",
        BackgroundKind::Criminal => "char_selection-background_detalle_criminal",
        BackgroundKind::Charlatan => "char_selection-background_detalle_charlatan",
        BackgroundKind::BountyHunter => "char_selection-background_detalle_bounty_hunter",
        BackgroundKind::Noble => "char_selection-background_detalle_noble",
        BackgroundKind::Entertainer => "char_selection-background_detalle_entertainer",
        BackgroundKind::FolkHero => "char_selection-background_detalle_folk_hero",
        BackgroundKind::Merchant => "char_selection-background_detalle_merchant",
        BackgroundKind::Artisan => "char_selection-background_detalle_artisan",
        BackgroundKind::Farmer => "char_selection-background_detalle_farmer",
        BackgroundKind::Fisher => "char_selection-background_detalle_fisher",
        BackgroundKind::Miner => "char_selection-background_detalle_miner",
        BackgroundKind::Outlander => "char_selection-background_detalle_outlander",
        BackgroundKind::Guide => "char_selection-background_detalle_guide",
        BackgroundKind::Sailor => "char_selection-background_detalle_sailor",
        BackgroundKind::Urchin => "char_selection-background_detalle_urchin",
    }
}

/// BL-31 UI-fixes (spec §4.4/§4.5): the starting-kit flavor description for
/// each background, transcribed from the triage doc's "Starting kit summary"
/// table. These are flavor/text descriptions only — the actual kit *items*
/// don't exist as real game assets yet (background-kit granting remains a
/// P3 stub, see `server::character_creator::apply_background_kit`).
fn background_starter_kit(kind: BackgroundKind) -> &'static str {
    match kind {
        BackgroundKind::Acolyte => "Holy symbol of the player's faith, 2x candle, prayer book",
        BackgroundKind::Hermit => "Scroll of personal discovery (flavor), pouch of herbs, blanket",
        BackgroundKind::Inquisitor => {
            "Writ of hunting authority (flavor document), 1x oil flask, manacles"
        },
        BackgroundKind::Sage => "2x blank tome, ink + quill, letter of introduction to a library",
        BackgroundKind::Archaeologist => "Bullseye lantern, 10-foot pole, rope (50ft), small tent",
        BackgroundKind::Scribe => "3x blank tome, set of inks, wax seal kit",
        BackgroundKind::Investigator => {
            "Magnifying glass, 2x paper sheets, hand-drawn map (flavor)"
        },
        BackgroundKind::Soldier => {
            "Campaign medal (flavor), insignia of rank (flavor), set of dice (gambling)"
        },
        BackgroundKind::Guard => "Whistle, club (if not already in class kit), badge (flavor)",
        BackgroundKind::Criminal => "Crowbar, dark hooded cloak, dice set",
        BackgroundKind::Charlatan => {
            "Disguise kit (flavor tool), 2x false documents, fine clothing (1 set)"
        },
        BackgroundKind::BountyHunter => "Manacles, bounty document (flavor), dark clothing",
        BackgroundKind::Noble => "Signet ring, letter of lineage (flavor), fine clothing set",
        BackgroundKind::Entertainer => {
            "Instrument (one type, flavor-only until instruments are implemented), costume, makeup \
             kit"
        },
        BackgroundKind::FolkHero => {
            "Shovel or pitchfork (or equivalent simple tool), hand-carved token (flavor)"
        },
        BackgroundKind::Merchant => {
            "Balance scales (flavor), 2x blank ledger pages, small coin purse"
        },
        BackgroundKind::Artisan => {
            "Personal craft tool set (flavor), samples of prior work (flavor), 2gp equiv coin"
        },
        BackgroundKind::Farmer => {
            "Shovel, 1x small animal produce (flavor consumable), work gloves (flavor)"
        },
        BackgroundKind::Fisher => "Fishing kit, rope (30ft), 1x salt-preserved food (consumable)",
        BackgroundKind::Miner => {
            "Mining pick (or use existing pickaxe), hooded lantern, 10x iron spikes"
        },
        BackgroundKind::Outlander => "Hunting trap, 1x staff (if not class kit), 2x trail rations",
        BackgroundKind::Guide => "Hand-drawn regional map (flavor), rope (30ft), signal whistle",
        BackgroundKind::Sailor => {
            "Rope (50ft), navigation charts (flavor), belaying pin (improvised weapon)"
        },
        BackgroundKind::Urchin => {
            "Small knife (if not in class kit), 1x city district map (flavor), lucky token"
        },
    }
}

/// BL-31 V2 (spec §1): the background pre-selected when the wizard's
/// Background step first renders. The grid now displays alphabetically by
/// `display_name()`, so the pre-selection must match whichever kind is
/// alphabetically first (top-left cell) rather than `BackgroundKind::ALL[0]`
/// (enum declaration order) — otherwise the highlighted grid cell wouldn't be
/// the one shown selected on first render. `BackgroundKind::ALL` is
/// non-empty (`ALL.len() == 24`, guarded by a unit test), so this always
/// returns `Some`.
fn alphabetically_first_background() -> BackgroundKind {
    BackgroundKind::ALL
        .into_iter()
        .min_by_key(|kind| kind.display_name())
        .expect("BackgroundKind::ALL is never empty")
}

/// BL-31 UI-fixes (spec §4.3): the companion detail panel for the
/// currently-selected background, shown in the right column while on the
/// Background step. Rebuilt from `background.0` every `view()` pass, so it
/// live-updates as the player clicks around the grid — no extra wiring
/// needed (iced immediate-mode). `kind` is always `Some(_)` in practice once
/// the wizard's Background step has rendered (spec §1's pre-selection
/// invariant), but `None` is handled defensively (empty panel) rather than
/// panicking, since this reads the same `Option<BackgroundKind>` as the
/// data-layer `Background` component.
fn background_detail_panel<'a>(
    kind: Option<BackgroundKind>,
    i18n: &Localization,
    fonts: &Fonts,
) -> Vec<Element<'a, Message>> {
    let Some(kind) = kind else {
        return Vec::new();
    };

    let heading = |text: String| -> Element<'a, Message> {
        Text::new(text)
            .size(fonts.cyri.scale(22))
            .color(Color::from_rgb(0.93, 0.78, 0.28))
            .into()
    };
    let body = |text: String| -> Element<'a, Message> {
        Text::new(text)
            .size(fonts.cyri.scale(18))
            .color(TEXT_COLOR)
            .into()
    };
    // Sections 2-4 (spec §4.4) have no authored lore yet; the placeholder is
    // styled dimmed/muted so it clearly reads as intentional, not a bug.
    let placeholder = |text: String| -> Element<'a, Message> {
        Text::new(text)
            .size(fonts.cyri.scale(16))
            .color(Color::from_rgba(1.0, 1.0, 1.0, 0.5))
            .into()
    };
    let section = |label_key: &str, content: Element<'a, Message>| -> Element<'a, Message> {
        Column::with_children(vec![heading(i18n.get_msg(label_key).into_owned()), content])
            .spacing(4)
            .width(Length::Fill)
            .into()
    };

    vec![
        // 1. Nombre del background — REAL (display_name()).
        section(
            "char_selection-background_detail_name",
            body(kind.display_name()),
        ),
        // 2. Detalle — REAL (BL-31 V2 spec §4/§5, verbatim from
        // docs/design/lore/chargen/21-background-detalle.md via i18n).
        section(
            "char_selection-background_detail_lore",
            body(i18n.get_msg(background_detalle(kind)).into_owned()),
        ),
        // 3. Beneficios e Interacciones Sociales — PLACEHOLDER.
        section(
            "char_selection-background_detail_social",
            placeholder(
                i18n.get_msg("char_selection-background_social_pending")
                    .into_owned(),
            ),
        ),
        // 4. Te codeas mejor con... — PLACEHOLDER.
        section(
            "char_selection-background_detail_affinity",
            placeholder(
                i18n.get_msg("char_selection-background_affinity_pending")
                    .into_owned(),
            ),
        ),
        // 5. Habilidades — REAL (triage doc stat passives, now two per
        // BL-31 V2 spec §3.1/§3.3), plus the "Sociedad: <label>" flavor line
        // (spec §3.2 — kept inside this section rather than as a 7th
        // top-level section, to avoid worsening the panel's vertical
        // pressure).
        section(
            "char_selection-background_detail_skills",
            Column::with_children(vec![
                body(background_stat_passive(kind).to_string()),
                body(format!(
                    "{}: {}",
                    i18n.get_msg("char_selection-background_society_label"),
                    background_society_type(kind)
                )),
            ])
            .spacing(2)
            .into(),
        ),
        // 6. Items (Starter Kit) — REAL (triage doc kit description).
        section(
            "char_selection-background_detail_kit",
            body(background_starter_kit(kind).to_string()),
        ),
    ]
}

// TODO: what does this comment mean?
// // Use in future MR to make this a starter weapon

// TODO: use for info popup frame/background
const UI_MAIN: Rgba<u8> = Rgba::new(156, 179, 179, 255); // Greenish Blue

image_ids_ice! {
    struct Imgs {
        <ImageGraphic>
        frame_bottom: "voxygen.element.ui.generic.frames.banner_bot",

        slider_range: "voxygen.element.ui.generic.slider.track",
        slider_indicator: "voxygen.element.ui.generic.slider.indicator",

        char_selection: "voxygen.element.ui.generic.frames.selection",
        char_selection_hover: "voxygen.element.ui.generic.frames.selection_hover",
        char_selection_press: "voxygen.element.ui.generic.frames.selection_press",

        delete_button: "voxygen.element.ui.char_select.icons.bin",
        delete_button_hover: "voxygen.element.ui.char_select.icons.bin_hover",
        delete_button_press: "voxygen.element.ui.char_select.icons.bin_press",

        edit_button: "voxygen.element.ui.char_select.icons.pen",
        edit_button_hover: "voxygen.element.ui.char_select.icons.pen_hover",
        edit_button_press: "voxygen.element.ui.char_select.icons.pen_press",

        name_input: "voxygen.element.ui.generic.textbox",

        // Tool Icons
        swords: "voxygen.element.weapons.swords",
        sword: "voxygen.element.weapons.sword",
        axe: "voxygen.element.weapons.axe",
        hammer: "voxygen.element.weapons.hammer",
        bow: "voxygen.element.weapons.bow",
        staff: "voxygen.element.weapons.staff",
        sceptre: "voxygen.element.weapons.sceptre",

        // Hardcore icon
        hardcore: "voxygen.element.ui.map.icons.dif_map_icon",

        // Dice icons
        dice: "voxygen.element.ui.char_select.icons.dice",
        dice_hover: "voxygen.element.ui.char_select.icons.dice_hover",
        dice_press: "voxygen.element.ui.char_select.icons.dice_press",

        // Species Icons
        human_m: "voxygen.element.ui.char_select.portraits.human_m",
        human_f: "voxygen.element.ui.char_select.portraits.human_f",
        orc_m: "voxygen.element.ui.char_select.portraits.orc_m",
        orc_f: "voxygen.element.ui.char_select.portraits.orc_f",
        dwarf_m: "voxygen.element.ui.char_select.portraits.dwarf_m",
        dwarf_f: "voxygen.element.ui.char_select.portraits.dwarf_f",
        draugr_m: "voxygen.element.ui.char_select.portraits.ud_m",
        draugr_f: "voxygen.element.ui.char_select.portraits.ud_f",
        elf_m: "voxygen.element.ui.char_select.portraits.elf_m",
        elf_f: "voxygen.element.ui.char_select.portraits.elf_f",
        danari_m: "voxygen.element.ui.char_select.portraits.danari_m",
        danari_f: "voxygen.element.ui.char_select.portraits.danari_f",
        // Icon Borders
        icon_border: "voxygen.element.ui.generic.buttons.border",
        icon_border_mo: "voxygen.element.ui.generic.buttons.border_mo",
        icon_border_press: "voxygen.element.ui.generic.buttons.border_press",
        icon_border_pressed: "voxygen.element.ui.generic.buttons.border_pressed",

        button: "voxygen.element.ui.generic.buttons.button",
        button_hover: "voxygen.element.ui.generic.buttons.button_hover",
        button_press: "voxygen.element.ui.generic.buttons.button_press",

        // Tooltips
        tt_edge: "voxygen.element.ui.generic.frames.tooltip.edge",
        tt_corner: "voxygen.element.ui.generic.frames.tooltip.corner",

        // Startzone Selection
        town_marker: "voxygen.element.ui.char_select.icons.town_marker",
    }
}

pub enum Event {
    Logout,
    Play(CharacterId),
    Spectate,
    AddCharacter {
        alias: String,
        mainhand: Option<String>,
        offhand: Option<String>,
        body: comp::Body,
        hardcore: bool,
        start_site: Option<SiteId>,
        class: ClassKind,
        ethos: Ethos,
        // BL-31: background chosen in the wizard's Background step.
        background: Background,
    },
    EditCharacter {
        alias: String,
        character_id: CharacterId,
        body: comp::Body,
    },
    DeleteCharacter(CharacterId),
    ClearCharacterListError,
    SelectCharacter(Option<CharacterId>),
    ShowRules,
}

#[expect(clippy::large_enum_variant)]
enum Mode {
    Select {
        info_content: Option<InfoContent>,

        characters_scroll: scrollable::State,
        character_buttons: Vec<button::State>,
        new_character_button: button::State,
        logout_button: button::State,
        rule_button: button::State,
        enter_world_button: button::State,
        spectate_button: button::State,
        yes_button: button::State,
        no_button: button::State,
    },
    CreateOrEdit {
        name: String,
        body: humanoid::Body,
        inventory: Box<Inventory>,
        mainhand: Option<&'static str>,
        offhand: Option<&'static str>,
        class: ClassKind,
        /// BL-33: the starting moral alignment chosen at creation.
        ethos: Ethos,
        /// BL-31: the background chosen at creation. During the creation
        /// wizard this is always `Some(_)` — the Background step pre-selects
        /// the alphabetically-first background (BL-31 V2 spec §1; matches
        /// the grid's alphabetical top-left cell) and every click always
        /// selects (never toggles off), so exactly one background is
        /// selected at all times (UI-fixes spec §1). `Background(None)`
        /// ("Uncommitted", P0 §Q1)
        /// remains a valid data-layer state for legacy characters, which
        /// don't run the creation wizard.
        background: Background,

        body_type_buttons: [button::State; 2],
        species_buttons: [button::State; 6],
        class_buttons: [button::State; 4],
        tool_buttons: [button::State; 6],
        ethos_moral_buttons: [button::State; 3],
        ethos_order_buttons: [button::State; 3],
        /// BL-31: one button per `BackgroundKind::ALL` entry, resized on
        /// first use (mirrors `character_buttons`).
        background_buttons: Vec<button::State>,
        background_scroll: scrollable::State,
        sliders: Sliders,
        hardcore_enabled: bool,
        left_scroll: scrollable::State,
        right_scroll: scrollable::State,
        name_input: text_input::State,
        back_button: button::State,
        create_button: button::State,
        rand_character_button: button::State,
        rand_name_button: button::State,
        prev_starting_site_button: button::State,
        next_starting_site_button: button::State,
        wizard_back_button: button::State,
        wizard_next_button: button::State,
        /// Current step of the creation wizard. Unused in edit mode.
        step: CreationStep,
        /// `character_id.is_some()` can be used to determine if we're in edit
        /// mode as opposed to create mode.
        // TODO: Something less janky? Express the problem domain better!
        character_id: Option<CharacterId>,
        start_site_idx: Option<usize>,
    },
}

/// Sequential steps of the character-creation wizard (creation mode only).
/// Edit mode ignores this and renders a single combined screen.
#[derive(Clone, Copy, PartialEq, Eq)]
enum CreationStep {
    Body,
    Appearance,
    Class,
    Alignment,
    /// BL-31 P0 §Q3: locked after Alignment, before Finish.
    Background,
    Finish,
}

impl CreationStep {
    fn next(self) -> Self {
        match self {
            CreationStep::Body => CreationStep::Appearance,
            CreationStep::Appearance => CreationStep::Class,
            CreationStep::Class => CreationStep::Alignment,
            CreationStep::Alignment => CreationStep::Background,
            CreationStep::Background => CreationStep::Finish,
            CreationStep::Finish => CreationStep::Finish,
        }
    }

    fn back(self) -> Self {
        match self {
            CreationStep::Body => CreationStep::Body,
            CreationStep::Appearance => CreationStep::Body,
            CreationStep::Class => CreationStep::Appearance,
            CreationStep::Alignment => CreationStep::Class,
            CreationStep::Background => CreationStep::Alignment,
            CreationStep::Finish => CreationStep::Background,
        }
    }

    /// 1-based index for the progress label.
    fn index(self) -> u8 {
        match self {
            CreationStep::Body => 1,
            CreationStep::Appearance => 2,
            CreationStep::Class => 3,
            CreationStep::Alignment => 4,
            CreationStep::Background => 5,
            CreationStep::Finish => 6,
        }
    }
}

impl Mode {
    pub fn select(info_content: Option<InfoContent>) -> Self {
        Self::Select {
            info_content,
            characters_scroll: Default::default(),
            character_buttons: Vec::new(),
            new_character_button: Default::default(),
            logout_button: Default::default(),
            rule_button: Default::default(),
            enter_world_button: Default::default(),
            spectate_button: Default::default(),
            yes_button: Default::default(),
            no_button: Default::default(),
        }
    }

    pub fn create(name: String) -> Self {
        // TODO: Load these from the server (presumably from a .ron) to allow for easier
        // modification of custom starting weapons
        let mainhand = Some(STARTER_SWORD);
        let offhand = None;

        let loadout = LoadoutBuilder::empty()
            .defaults()
            .active_mainhand(mainhand.map(Item::new_from_asset_expect))
            .active_offhand(offhand.map(Item::new_from_asset_expect))
            .build();

        let inventory = Box::new(Inventory::with_loadout_humanoid(loadout));

        Self::CreateOrEdit {
            name,
            body: humanoid::Body::random(),
            inventory,
            mainhand,
            offhand,
            class: ClassKind::Warrior,
            ethos: Ethos::default(),
            // BL-31 UI-fixes spec §1, updated by V2 spec §1: the creation
            // wizard never renders the Background step with nothing
            // selected; seed the alphabetically-first background so it
            // matches the grid's alphabetical top-left cell (count-agnostic
            // — spec §0.1).
            background: Background(Some(alphabetically_first_background())),
            body_type_buttons: Default::default(),
            species_buttons: Default::default(),
            class_buttons: Default::default(),
            tool_buttons: Default::default(),
            ethos_moral_buttons: Default::default(),
            ethos_order_buttons: Default::default(),
            background_buttons: Vec::new(),
            background_scroll: Default::default(),
            sliders: Default::default(),
            hardcore_enabled: false,
            left_scroll: Default::default(),
            right_scroll: Default::default(),
            name_input: Default::default(),
            back_button: Default::default(),
            create_button: Default::default(),
            rand_character_button: Default::default(),
            rand_name_button: Default::default(),
            prev_starting_site_button: Default::default(),
            next_starting_site_button: Default::default(),
            wizard_back_button: Default::default(),
            wizard_next_button: Default::default(),
            step: CreationStep::Body,
            character_id: None,
            start_site_idx: None,
        }
    }

    pub fn edit(
        name: String,
        character_id: CharacterId,
        body: humanoid::Body,
        inventory: &Inventory,
    ) -> Self {
        Self::CreateOrEdit {
            name,
            body,
            inventory: Box::new(inventory.clone()),
            mainhand: None,
            offhand: None,
            class: ClassKind::Adventurer,
            ethos: Ethos::default(),
            // BL-31 UI-fixes spec §1, updated by V2 spec §1: the creation
            // wizard never renders the Background step with nothing
            // selected; seed the alphabetically-first background so it
            // matches the grid's alphabetical top-left cell (count-agnostic
            // — spec §0.1).
            background: Background(Some(alphabetically_first_background())),
            body_type_buttons: Default::default(),
            species_buttons: Default::default(),
            class_buttons: Default::default(),
            tool_buttons: Default::default(),
            ethos_moral_buttons: Default::default(),
            ethos_order_buttons: Default::default(),
            background_buttons: Vec::new(),
            background_scroll: Default::default(),
            sliders: Default::default(),
            hardcore_enabled: false,
            left_scroll: Default::default(),
            right_scroll: Default::default(),
            name_input: Default::default(),
            back_button: Default::default(),
            create_button: Default::default(),
            rand_character_button: Default::default(),
            rand_name_button: Default::default(),
            prev_starting_site_button: Default::default(),
            next_starting_site_button: Default::default(),
            wizard_back_button: Default::default(),
            wizard_next_button: Default::default(),
            // Unused in edit mode (single combined screen).
            step: CreationStep::Body,
            character_id: Some(character_id),
            start_site_idx: None,
        }
    }
}

#[derive(PartialEq)]
enum InfoContent {
    Deletion(usize),
    LoadingCharacters,
    CreatingCharacter,
    EditingCharacter,
    JoiningCharacter,
    CharacterError(String),
}

struct Controls {
    fonts: Fonts,
    imgs: Imgs,
    // Voxygen version
    version: String,
    server_mismatched_version: Option<String>,
    tooltip_manager: TooltipManager,
    // Zone for rotating the character with the mouse
    mouse_detector: mouse_detector::State,
    mode: Mode,
    // Id of the selected character
    selected: Option<CharacterId>,
    default_name: String,
    map_img: GraphicId,
    possible_starting_sites: Vec<Marker>,
    world_sz: Vec2<u32>,
    has_rules: bool,
}

#[derive(Clone)]
enum Message {
    Back,
    Logout,
    ShowRules,
    EnterWorld,
    Spectate,
    Select(CharacterId),
    Delete(usize),
    Edit(usize),
    ConfirmEdit(CharacterId),
    NewCharacter,
    CreateCharacter,
    Name(String),
    BodyType(humanoid::BodyType),
    Species(humanoid::Species),
    Class(ClassKind),
    EthosMoral(Moral),
    EthosOrder(Order),
    /// BL-31: select a background. The creation wizard's Background step is
    /// a single-select radio group — clicking any entry always selects it
    /// (UI-fixes spec §1); `None` remains reachable only as the legacy
    /// "Uncommitted" data-layer state (never sent by the wizard).
    Background(Option<BackgroundKind>),
    Tool((Option<&'static str>, Option<&'static str>)),
    RandomizeCharacter,
    HardcoreEnabled(bool),
    RandomizeName,
    CancelDeletion,
    ConfirmDeletion,
    ClearCharacterListError,
    HairStyle(u8),
    HairColor(u8),
    Skin(u8),
    Eyes(u8),
    EyeColor(u8),
    Accessory(u8),
    Beard(u8),
    StartingSite(usize),
    PrevStartingSite,
    NextStartingSite,
    WizardNext,
    WizardBack,
    // Workaround for widgets that require a message but we don't want them to actually do
    // anything
    DoNothing,
}

impl Controls {
    fn new(
        fonts: Fonts,
        imgs: Imgs,
        selected: Option<CharacterId>,
        default_name: String,
        server_info: &ServerInfo,
        map_img: GraphicId,
        possible_starting_sites: Vec<Marker>,
        world_sz: Vec2<u32>,
        has_rules: bool,
    ) -> Self {
        let version = format!("Veloren {}", *common::util::DISPLAY_VERSION);
        let server_mismatched_version = (*common::util::GIT_HASH != server_info.git_hash
            || *common::util::GIT_TIMESTAMP != server_info.git_timestamp)
            .then(|| {
                common::util::make_display_version(server_info.git_hash, server_info.git_timestamp)
            });

        Self {
            fonts,
            imgs,
            version,
            server_mismatched_version,
            tooltip_manager: TooltipManager::new(TOOLTIP_HOVER_DUR, TOOLTIP_FADE_DUR),
            mouse_detector: Default::default(),
            mode: Mode::select(Some(InfoContent::LoadingCharacters)),
            selected,
            default_name,
            map_img,
            possible_starting_sites,
            world_sz,
            has_rules,
        }
    }

    fn view<'a>(
        &'a mut self,
        _settings: &Settings,
        client: &Client,
        error: &Option<String>,
        i18n: &'a Localization,
    ) -> Element<'a, Message> {
        // TODO: use font scale thing for text size (use on button size for buttons with
        // text)

        // Maintain tooltip manager
        self.tooltip_manager.maintain();

        let imgs = &self.imgs;
        let fonts = &self.fonts;
        let tooltip_manager = &self.tooltip_manager;

        let button_style = style::button::Style::new(imgs.button)
            .hover_image(imgs.button_hover)
            .press_image(imgs.button_press)
            .text_color(TEXT_COLOR)
            .disabled_text_color(DISABLED_TEXT_COLOR);

        let tooltip_style = tooltip::Style {
            container: style::container::Style::color_with_image_border(
                TOOLTIP_BACK_COLOR,
                imgs.tt_corner,
                imgs.tt_edge,
            ),
            text_color: TEXT_COLOR,
            text_size: self.fonts.cyri.scale(17),
            padding: 10,
        };

        let version = Text::new(&self.version)
            .size(self.fonts.cyri.scale(12))
            .width(Length::Fill)
            .horizontal_alignment(HorizontalAlignment::Center);

        let top_text = Row::with_children(vec![
            Space::new(Length::Fill, Length::Shrink).into(),
            version.into(),
            Space::new(Length::Fill, Length::Shrink).into(),
        ])
        .width(Length::Fill);

        let mut warning_container = if let Some(mismatched_version) =
            &self.server_mismatched_version
        {
            let warning = Text::<IcedRenderer>::new(format!(
                "{}\n{}: {} {}: {}",
                i18n.get_msg("char_selection-version_mismatch"),
                i18n.get_msg("main-login-server_version"),
                mismatched_version,
                i18n.get_msg("main-login-client_version"),
                *common::util::DISPLAY_VERSION
            ))
            .size(self.fonts.cyri.scale(18))
            .color(iced::Color::from_rgb(1.0, 0.0, 0.0))
            .width(Length::Fill)
            .horizontal_alignment(HorizontalAlignment::Center);
            Some(
                Container::new(
                    Container::new(Row::with_children(vec![warning.into()]).width(Length::Fill))
                        .style(style::container::Style::color(Rgba::new(0, 0, 0, 217)))
                        .padding(12)
                        .width(Length::Fill)
                        .center_x(),
                )
                .padding(16),
            )
        } else {
            None
        };

        let content = match &mut self.mode {
            Mode::Select {
                info_content,
                characters_scroll,
                character_buttons,
                new_character_button,
                logout_button,
                rule_button,
                enter_world_button,
                spectate_button,
                yes_button,
                no_button,
            } => {
                match self.selected {
                    Some(character_id) => {
                        // If the selected character no longer exists, deselect it.
                        if !client
                            .character_list()
                            .characters
                            .iter()
                            .any(|char| char.character.id == Some(character_id))
                        {
                            self.selected = None;
                        }
                    },
                    None => {
                        // If no character is selected then select the first one
                        // Note: we don't need to persist this because it is the default
                        self.selected = client
                            .character_list()
                            .characters
                            .first()
                            .and_then(|i| i.character.id);
                    },
                }

                // Get the index of the selected character
                let selected = self.selected.and_then(|id| {
                    client
                        .character_list()
                        .characters
                        .iter()
                        .position(|i| i.character.id == Some(id))
                });

                if let Some(error) = error {
                    // TODO: use more user friendly errors with suggestions on potential solutions
                    // instead of directly showing error message here
                    *info_content = Some(InfoContent::CharacterError(format!(
                        "{}: {}",
                        i18n.get_msg("common-error"),
                        error
                    )))
                } else if let Some(InfoContent::CharacterError(_)) = info_content {
                    *info_content = None;
                } else if matches!(
                    info_content,
                    Some(InfoContent::LoadingCharacters)
                        | Some(InfoContent::CreatingCharacter)
                        | Some(InfoContent::EditingCharacter)
                ) && !client.character_list().loading
                {
                    *info_content = None;
                }

                #[cfg(feature = "singleplayer")]
                let server_name =
                    if client.server_info().name == server::settings::SINGLEPLAYER_SERVER_NAME {
                        &i18n.get_msg("common-singleplayer").to_string()
                    } else {
                        &client.server_info().name
                    };
                #[cfg(not(feature = "singleplayer"))]
                let server_name = &client.server_info().name;

                let server = Container::new(
                    Column::with_children(vec![
                        Text::new(server_name).size(fonts.cyri.scale(25)).into(),
                        // TODO: show additional server info here
                        Space::new(Length::Fill, Length::Units(25)).into(),
                    ])
                    .spacing(5)
                    .align_items(Align::Center),
                )
                .style(style::container::Style::color(Rgba::new(0, 0, 0, 217)))
                .padding(12)
                .center_x()
                .center_y()
                .width(Length::Fill);

                let characters = {
                    let characters = &client.character_list().characters;
                    let num = characters.len();
                    // Ensure we have enough button states
                    const CHAR_BUTTONS: usize = 3;
                    character_buttons.resize_with(num * CHAR_BUTTONS, Default::default);

                    // Character Selection List
                    let mut characters = characters
                        .iter()
                        .zip(
                            character_buttons
                                .as_chunks_mut::<CHAR_BUTTONS>()
                                .0
                                .iter_mut(),
                        )
                        .filter_map(|(character, buttons)| {
                            let mut buttons = buttons.iter_mut();
                            // TODO: eliminate option in character id?
                            character.character.id.map(|id| {
                                (
                                    id,
                                    character,
                                    (
                                        buttons.next().unwrap(),
                                        buttons.next().unwrap(),
                                        buttons.next().unwrap(),
                                    ),
                                )
                            })
                        })
                        .enumerate()
                        .map(
                            |(
                                i,
                                (
                                    character_id,
                                    character,
                                    (select_button, edit_button, delete_button),
                                ),
                            )| {
                                let select_col = if Some(i) == selected {
                                    (255, 208, 69)
                                } else {
                                    (255, 255, 255)
                                };
                                Overlay::new(
                                    Container::new(Column::with_children({
                                        let mut elements = Vec::new();
                                        if character.hardcore {
                                            elements.push(
                                                Image::new(imgs.hardcore)
                                                    .width(Length::Units(32))
                                                    .height(Length::Units(32))
                                                    .into(),
                                            );
                                        }
                                        elements.push(
                                            Row::with_children(vec![
                                                // Edit button
                                                Button::new(
                                                    edit_button,
                                                    Space::new(
                                                        Length::Units(16),
                                                        Length::Units(16),
                                                    ),
                                                )
                                                .style(
                                                    style::button::Style::new(imgs.edit_button)
                                                        .hover_image(imgs.edit_button_hover)
                                                        .press_image(imgs.edit_button_press),
                                                )
                                                .on_press(Message::Edit(i))
                                                .into(),
                                                // Delete button
                                                Button::new(
                                                    delete_button,
                                                    Space::new(
                                                        Length::Units(16),
                                                        Length::Units(16),
                                                    ),
                                                )
                                                .style(
                                                    style::button::Style::new(imgs.delete_button)
                                                        .hover_image(imgs.delete_button_hover)
                                                        .press_image(imgs.delete_button_press),
                                                )
                                                .on_press(Message::Delete(i))
                                                .into(),
                                            ])
                                            .spacing(5)
                                            .into(),
                                        );

                                        elements
                                    }))
                                    .padding(4),
                                    // Select Button
                                    AspectRatioContainer::new(
                                        Button::new(
                                            select_button,
                                            Column::with_children(vec![
                                                Text::new(&character.character.alias)
                                                    .size(fonts.cyri.scale(26))
                                                    .into(),
                                                Text::new(character.location.as_ref().map_or_else(
                                                    || {
                                                        i18n.get_msg(
                                                            "char_selection-uncanny_valley",
                                                        )
                                                        .to_string()
                                                    },
                                                    |s| s.clone(),
                                                ))
                                                .into(),
                                            ]),
                                        )
                                        .padding(10)
                                        .style(
                                            style::button::Style::new(if Some(i) == selected {
                                                imgs.char_selection_hover
                                            } else {
                                                imgs.char_selection
                                            })
                                            .hover_image(imgs.char_selection_hover)
                                            .press_image(imgs.char_selection_press)
                                            .image_color(Rgba::new(
                                                select_col.0,
                                                select_col.1,
                                                select_col.2,
                                                255,
                                            )),
                                        )
                                        .width(Length::Fill)
                                        .height(Length::Fill)
                                        .on_press(Message::Select(character_id)),
                                    )
                                    .ratio_of_image(imgs.char_selection),
                                )
                                .padding(0)
                                .align_x(Align::End)
                                .align_y(Align::End)
                                .into()
                            },
                        )
                        .collect::<Vec<_>>();

                    // Add create new character button
                    let color = if num >= MAX_CHARACTERS_PER_PLAYER {
                        (97, 97, 25)
                    } else {
                        (97, 255, 18)
                    };
                    characters.push(
                        AspectRatioContainer::new({
                            let button = Button::new(
                                new_character_button,
                                Container::new(Text::new(
                                    i18n.get_msg("char_selection-create_new_character"),
                                ))
                                .width(Length::Fill)
                                .height(Length::Fill)
                                .center_x()
                                .center_y(),
                            )
                            .style(
                                style::button::Style::new(imgs.char_selection)
                                    .hover_image(imgs.char_selection_hover)
                                    .press_image(imgs.char_selection_press)
                                    .image_color(Rgba::new(color.0, color.1, color.2, 255))
                                    .text_color(iced::Color::from_rgb8(color.0, color.1, color.2))
                                    .disabled_text_color(iced::Color::from_rgb8(
                                        color.0, color.1, color.2,
                                    )),
                            )
                            .width(Length::Fill)
                            .height(Length::Fill);
                            if num < MAX_CHARACTERS_PER_PLAYER {
                                button.on_press(Message::NewCharacter)
                            } else {
                                button
                            }
                        })
                        .ratio_of_image(imgs.char_selection)
                        .into(),
                    );
                    characters
                };

                // TODO: could replace column with scrollable completely if it had a with
                // children method
                let characters = Column::with_children(vec![
                    Container::new(
                        Scrollable::new(characters_scroll)
                            .push(Column::with_children(characters).spacing(4))
                            .padding(6)
                            .scrollbar_width(5)
                            .scroller_width(5)
                            .width(Length::Fill)
                            .style(style::scrollable::Style {
                                track: None,
                                scroller: style::scrollable::Scroller::Color(UI_MAIN),
                            }),
                    )
                    .style(style::container::Style::color(Rgba::from_translucent(
                        0,
                        BANNER_ALPHA,
                    )))
                    .width(Length::Units(322))
                    .height(Length::Fill)
                    .center_x()
                    .into(),
                    Image::new(imgs.frame_bottom)
                        .height(Length::Units(40))
                        .width(Length::Units(322))
                        .color(Rgba::from_translucent(0, BANNER_ALPHA))
                        .into(),
                ])
                .height(Length::Fill);

                let mut left_column_children = vec![server.into(), characters.into()];

                if self.has_rules {
                    left_column_children.push(
                        Container::new(neat_button(
                            rule_button,
                            i18n.get_msg("char_selection-rules").into_owned(),
                            FILL_FRAC_ONE,
                            button_style,
                            Some(Message::ShowRules),
                        ))
                        .align_y(Align::End)
                        .width(Length::Fill)
                        .center_x()
                        .height(Length::Units(52))
                        .into(),
                    );
                }
                let left_column = Column::with_children(left_column_children)
                    .spacing(10)
                    .width(Length::Units(322)) // TODO: see if we can get iced to work with settings below
                    // .max_width(360)
                    // .width(Length::Fill)
                    .height(Length::Fill);

                let top = Row::with_children(vec![
                    left_column.into(),
                    MouseDetector::new(&mut self.mouse_detector, Length::Fill, Length::Fill).into(),
                ])
                .padding(15)
                .width(Length::Fill)
                .height(Length::Fill);
                let mut bottom_content = vec![
                    Container::new(neat_button(
                        logout_button,
                        i18n.get_msg("char_selection-logout").into_owned(),
                        FILL_FRAC_ONE,
                        button_style,
                        Some(Message::Logout),
                    ))
                    .width(Length::Fill)
                    .height(Length::Units(SMALL_BUTTON_HEIGHT))
                    .into(),
                ];

                if client.is_moderator() && client.client_type().can_spectate() {
                    bottom_content.push(
                        Container::new(neat_button(
                            spectate_button,
                            i18n.get_msg("char_selection-spectate").into_owned(),
                            FILL_FRAC_TWO,
                            button_style,
                            Some(Message::Spectate),
                        ))
                        .width(Length::Fill)
                        .height(Length::Units(52))
                        .center_x()
                        .into(),
                    );
                }

                if client.client_type().can_enter_character() {
                    bottom_content.push(
                        Container::new(neat_button(
                            enter_world_button,
                            i18n.get_msg("char_selection-enter_world").into_owned(),
                            FILL_FRAC_TWO,
                            button_style,
                            selected.map(|_| Message::EnterWorld),
                        ))
                        .width(Length::Fill)
                        .height(Length::Units(52))
                        .center_x()
                        .into(),
                    );
                }

                bottom_content.push(Space::new(Length::Fill, Length::Shrink).into());

                let bottom = Row::with_children(bottom_content).align_items(Align::End);

                let content = Column::with_children(vec![top.into(), bottom.into()])
                    .width(Length::Fill)
                    .padding(5)
                    .height(Length::Fill);

                // Overlay delete prompt
                if let Some(info_content) = info_content {
                    let over_content: Element<_> = match &info_content {
                        InfoContent::Deletion(_) => Column::with_children(vec![
                            Text::new(i18n.get_msg("char_selection-delete_permanently"))
                                .size(fonts.cyri.scale(24))
                                .into(),
                            Row::with_children(vec![
                                neat_button(
                                    no_button,
                                    i18n.get_msg("common-no").into_owned(),
                                    FILL_FRAC_ONE,
                                    button_style,
                                    Some(Message::CancelDeletion),
                                ),
                                neat_button(
                                    yes_button,
                                    i18n.get_msg("common-yes").into_owned(),
                                    FILL_FRAC_ONE,
                                    button_style,
                                    Some(Message::ConfirmDeletion),
                                ),
                            ])
                            .height(Length::Units(28))
                            .spacing(30)
                            .into(),
                        ])
                        .align_items(Align::Center)
                        .spacing(10)
                        .into(),
                        InfoContent::LoadingCharacters => {
                            Text::new(i18n.get_msg("char_selection-loading_characters"))
                                .size(fonts.cyri.scale(24))
                                .into()
                        },
                        InfoContent::CreatingCharacter => {
                            Text::new(i18n.get_msg("char_selection-creating_character"))
                                .size(fonts.cyri.scale(24))
                                .into()
                        },
                        InfoContent::EditingCharacter => {
                            Text::new(i18n.get_msg("char_selection-editing_character"))
                                .size(fonts.cyri.scale(24))
                                .into()
                        },
                        InfoContent::JoiningCharacter => {
                            Text::new(i18n.get_msg("char_selection-joining_character"))
                                .size(fonts.cyri.scale(24))
                                .into()
                        },
                        InfoContent::CharacterError(error) => Column::with_children(vec![
                            Text::new(error).size(fonts.cyri.scale(24)).into(),
                            Row::with_children(vec![neat_button(
                                no_button,
                                i18n.get_msg("common-close").into_owned(),
                                FILL_FRAC_ONE,
                                button_style,
                                Some(Message::ClearCharacterListError),
                            )])
                            .height(Length::Units(28))
                            .into(),
                        ])
                        .align_items(Align::Center)
                        .spacing(10)
                        .into(),
                    };

                    let over = Container::new(over_content)
                        .style(
                            style::container::Style::color_with_double_cornerless_border(
                                (0, 0, 0, 200).into(),
                                (3, 4, 4, 255).into(),
                                (28, 28, 22, 255).into(),
                            ),
                        )
                        .width(Length::Shrink)
                        .height(Length::Shrink)
                        .max_width(400)
                        .max_height(500)
                        .padding(24)
                        .center_x()
                        .center_y();

                    Overlay::new(over, content)
                        .width(Length::Fill)
                        .height(Length::Fill)
                        .center_x()
                        .center_y()
                        .into()
                } else {
                    content.into()
                }
            },
            Mode::CreateOrEdit {
                name,
                body,
                inventory: _,
                mainhand,
                offhand: _,
                class,
                ethos,
                background,
                left_scroll,
                right_scroll,
                body_type_buttons,
                species_buttons,
                class_buttons,
                tool_buttons,
                ethos_moral_buttons,
                ethos_order_buttons,
                background_buttons,
                background_scroll,
                sliders,
                hardcore_enabled,
                name_input,
                back_button,
                create_button,
                rand_character_button,
                rand_name_button,
                prev_starting_site_button,
                next_starting_site_button,
                wizard_back_button,
                wizard_next_button,
                step,
                character_id,
                start_site_idx,
            } => {
                // Copy the step out so the later `match` doesn't keep `step`
                // borrowed while we build widgets from the other fields.
                let step = *step;
                let unselected_style = style::button::Style::new(imgs.icon_border)
                    .hover_image(imgs.icon_border_mo)
                    .press_image(imgs.icon_border_press);

                let selected_style = style::button::Style::new(imgs.icon_border_pressed)
                    .hover_image(imgs.icon_border_mo)
                    .press_image(imgs.icon_border_press);

                let icon_button = |button, selected, msg, img| {
                    Container::new(
                        Button::<_, IcedRenderer>::new(
                            button,
                            Space::new(Length::Units(60), Length::Units(60)),
                        )
                        .style(if selected {
                            selected_style
                        } else {
                            unselected_style
                        })
                        .on_press(msg),
                    )
                    .style(style::container::Style::image(img))
                };
                let icon_button_tooltip = |button, selected, msg, img, tooltip_i18n_key| {
                    icon_button(button, selected, msg, img).with_tooltip(
                        tooltip_manager,
                        move || {
                            let tooltip_text = i18n.get_msg(tooltip_i18n_key);
                            tooltip::text(&tooltip_text, tooltip_style)
                        },
                    )
                };

                // TODO: tooltips
                let (tool, species, body_type, class_section) = if character_id.is_some() {
                    (Column::new(), Column::new(), Row::new(), Column::new())
                } else {
                    let (body_m_ico, body_f_ico) = match body.species {
                        humanoid::Species::Human => (imgs.human_m, imgs.human_f),
                        humanoid::Species::Orc => (imgs.orc_m, imgs.orc_f),
                        humanoid::Species::Dwarf => (imgs.dwarf_m, imgs.dwarf_f),
                        humanoid::Species::Elf => (imgs.elf_m, imgs.elf_f),
                        humanoid::Species::Draugr => (imgs.draugr_m, imgs.draugr_f),
                        humanoid::Species::Danari => (imgs.danari_m, imgs.danari_f),
                    };
                    let [body_m_button, body_f_button] = body_type_buttons;
                    let body_type = Row::with_children(vec![
                        icon_button(
                            body_m_button,
                            matches!(body.body_type, humanoid::BodyType::Male),
                            Message::BodyType(humanoid::BodyType::Male),
                            body_m_ico,
                        )
                        .into(),
                        icon_button(
                            body_f_button,
                            matches!(body.body_type, humanoid::BodyType::Female),
                            Message::BodyType(humanoid::BodyType::Female),
                            body_f_ico,
                        )
                        .into(),
                    ])
                    .spacing(1);
                    let (human_icon, orc_icon, dwarf_icon, elf_icon, draugr_icon, danari_icon) =
                        match body.body_type {
                            humanoid::BodyType::Male => (
                                self.imgs.human_m,
                                self.imgs.orc_m,
                                self.imgs.dwarf_m,
                                self.imgs.elf_m,
                                self.imgs.draugr_m,
                                self.imgs.danari_m,
                            ),
                            humanoid::BodyType::Female => (
                                self.imgs.human_f,
                                self.imgs.orc_f,
                                self.imgs.dwarf_f,
                                self.imgs.elf_f,
                                self.imgs.draugr_f,
                                self.imgs.danari_f,
                            ),
                        };
                    let [
                        human_button,
                        orc_button,
                        dwarf_button,
                        elf_button,
                        draugr_button,
                        danari_button,
                    ] = species_buttons;
                    let species = Column::with_children(vec![
                        Row::with_children(vec![
                            icon_button_tooltip(
                                human_button,
                                matches!(body.species, humanoid::Species::Human),
                                Message::Species(humanoid::Species::Human),
                                human_icon,
                                "common-species-human",
                            )
                            .into(),
                            icon_button_tooltip(
                                orc_button,
                                matches!(body.species, humanoid::Species::Orc),
                                Message::Species(humanoid::Species::Orc),
                                orc_icon,
                                "common-species-orc",
                            )
                            .into(),
                            icon_button_tooltip(
                                dwarf_button,
                                matches!(body.species, humanoid::Species::Dwarf),
                                Message::Species(humanoid::Species::Dwarf),
                                dwarf_icon,
                                "common-species-dwarf",
                            )
                            .into(),
                        ])
                        .spacing(1)
                        .into(),
                        Row::with_children(vec![
                            icon_button_tooltip(
                                elf_button,
                                matches!(body.species, humanoid::Species::Elf),
                                Message::Species(humanoid::Species::Elf),
                                elf_icon,
                                "common-species-elf",
                            )
                            .into(),
                            icon_button_tooltip(
                                draugr_button,
                                matches!(body.species, humanoid::Species::Draugr),
                                Message::Species(humanoid::Species::Draugr),
                                draugr_icon,
                                "common-species-draugr",
                            )
                            .into(),
                            icon_button_tooltip(
                                danari_button,
                                matches!(body.species, humanoid::Species::Danari),
                                Message::Species(humanoid::Species::Danari),
                                danari_icon,
                                "common-species-danari",
                            )
                            .into(),
                        ])
                        .spacing(1)
                        .into(),
                    ])
                    .spacing(1);
                    // Class picker: four text buttons, one per playable class.
                    let [
                        warrior_class_button,
                        mage_class_button,
                        cleric_class_button,
                        rogue_class_button,
                    ] = class_buttons;
                    // Selection is signalled ONLY by text color: every state
                    // shares the same button images, so the geometry never
                    // changes and the 2x2 grid stays stable under the cursor
                    // (selected-style image swaps caused reflow + misclicks).
                    let class_button_style = |selected: bool| {
                        if selected {
                            style::button::Style::new(imgs.button)
                                .hover_image(imgs.button_hover)
                                .press_image(imgs.button_press)
                                .text_color(Color::from_rgb(0.93, 0.78, 0.28))
                        } else {
                            button_style
                        }
                    };
                    // 2 per row: new classes extend downward, never sideways.
                    let class_section = Column::with_children(vec![
                        Row::with_children(vec![
                            neat_button(
                                warrior_class_button,
                                i18n.get_msg("char_selection-class_warrior").into_owned(),
                                FILL_FRAC_ONE,
                                class_button_style(*class == ClassKind::Warrior),
                                Some(Message::Class(ClassKind::Warrior)),
                            ),
                            neat_button(
                                mage_class_button,
                                i18n.get_msg("char_selection-class_mage").into_owned(),
                                FILL_FRAC_ONE,
                                class_button_style(*class == ClassKind::Mage),
                                Some(Message::Class(ClassKind::Mage)),
                            ),
                        ])
                        .height(Length::Units(26))
                        .spacing(2)
                        .into(),
                        Row::with_children(vec![
                            neat_button(
                                cleric_class_button,
                                i18n.get_msg("char_selection-class_cleric").into_owned(),
                                FILL_FRAC_ONE,
                                class_button_style(*class == ClassKind::Cleric),
                                Some(Message::Class(ClassKind::Cleric)),
                            ),
                            neat_button(
                                rogue_class_button,
                                i18n.get_msg("char_selection-class_rogue").into_owned(),
                                FILL_FRAC_ONE,
                                class_button_style(*class == ClassKind::Rogue),
                                Some(Message::Class(ClassKind::Rogue)),
                            ),
                        ])
                        .height(Length::Units(26))
                        .spacing(2)
                        .into(),
                    ])
                    .align_items(Align::Center)
                    .spacing(2);

                    // Tool buttons gated by the current class's whitelist.
                    // A button with no on_press is visually present but non-interactive.
                    let icon_button_opt = |button, selected, msg: Option<Message>, img| {
                        let btn = Button::<_, IcedRenderer>::new(
                            button,
                            Space::new(Length::Units(60), Length::Units(60)),
                        )
                        .style(if selected {
                            selected_style
                        } else {
                            unselected_style
                        });
                        let btn = match msg {
                            Some(m) => btn.on_press(m),
                            None => btn,
                        };
                        Container::new(btn).style(style::container::Style::image(img))
                    };
                    let icon_button_tooltip_opt =
                        |button, selected, msg: Option<Message>, img, tooltip_i18n_key| {
                            icon_button_opt(button, selected, msg, img).with_tooltip(
                                tooltip_manager,
                                move || {
                                    let tooltip_text = i18n.get_msg(tooltip_i18n_key);
                                    tooltip::text(&tooltip_text, tooltip_style)
                                },
                            )
                        };

                    // Weapon picker for step 3: render ONLY the weapons valid for
                    // the currently selected class. Every button gets a real
                    // `on_press` (no disabled/placeholder buttons).
                    let [
                        sword_button,
                        swords_button,
                        axe_button,
                        hammer_button,
                        bow_button,
                        staff_button,
                    ] = tool_buttons;
                    let tool = match *class {
                        ClassKind::Warrior | ClassKind::Adventurer => Column::with_children(vec![
                            Row::with_children(vec![
                                icon_button_tooltip_opt(
                                    sword_button,
                                    *mainhand == Some(STARTER_SWORD),
                                    Some(Message::Tool((Some(STARTER_SWORD), None))),
                                    imgs.sword,
                                    "common-weapons-greatsword",
                                )
                                .into(),
                                icon_button_tooltip_opt(
                                    hammer_button,
                                    *mainhand == Some(STARTER_HAMMER),
                                    Some(Message::Tool((Some(STARTER_HAMMER), None))),
                                    imgs.hammer,
                                    "common-weapons-hammer",
                                )
                                .into(),
                                icon_button_tooltip_opt(
                                    axe_button,
                                    *mainhand == Some(STARTER_AXE),
                                    Some(Message::Tool((Some(STARTER_AXE), None))),
                                    imgs.axe,
                                    "common-weapons-axe",
                                )
                                .into(),
                            ])
                            .spacing(1)
                            .into(),
                        ]),
                        // BL-04: all staff-starter casters share the staff picker.
                        ClassKind::Mage
                        | ClassKind::Sorcerer
                        | ClassKind::Warlock
                        | ClassKind::Bard
                        | ClassKind::Druid
                        | ClassKind::Artificer => Column::with_children(vec![
                            icon_button_tooltip_opt(
                                staff_button,
                                *mainhand == Some(STARTER_STAFF),
                                Some(Message::Tool((Some(STARTER_STAFF), None))),
                                imgs.staff,
                                "common-weapons-staff",
                            )
                            .into(),
                        ]),
                        ClassKind::Cleric => Column::with_children(vec![
                            icon_button_tooltip_opt(
                                staff_button,
                                *mainhand == Some(STARTER_SCEPTRE),
                                Some(Message::Tool((Some(STARTER_SCEPTRE), None))),
                                imgs.sceptre,
                                "common-weapons-sceptre",
                            )
                            .into(),
                        ]),
                        ClassKind::Rogue => Column::with_children(vec![
                            Row::with_children(vec![
                                icon_button_tooltip_opt(
                                    swords_button,
                                    *mainhand == Some(STARTER_SWORDS),
                                    Some(Message::Tool((
                                        Some(STARTER_SWORDS),
                                        Some(STARTER_SWORDS),
                                    ))),
                                    imgs.swords,
                                    "common-weapons-shortswords",
                                )
                                .into(),
                                icon_button_tooltip_opt(
                                    bow_button,
                                    *mainhand == Some(STARTER_BOW),
                                    Some(Message::Tool((Some(STARTER_BOW), None))),
                                    imgs.bow,
                                    "common-weapons-bow",
                                )
                                .into(),
                            ])
                            .spacing(1)
                            .into(),
                        ]),
                        // BL-04 classes-wave: pickers match each class' server whitelist.
                        ClassKind::Paladin | ClassKind::BloodSlayer => Column::with_children(vec![
                            icon_button_tooltip_opt(
                                sword_button,
                                *mainhand == Some(STARTER_SWORD),
                                Some(Message::Tool((Some(STARTER_SWORD), None))),
                                imgs.sword,
                                "common-weapons-greatsword",
                            )
                            .into(),
                        ]),
                        ClassKind::Ranger => Column::with_children(vec![
                            icon_button_tooltip_opt(
                                bow_button,
                                *mainhand == Some(STARTER_BOW),
                                Some(Message::Tool((Some(STARTER_BOW), None))),
                                imgs.bow,
                                "common-weapons-bow",
                            )
                            .into(),
                        ]),
                        ClassKind::Monk => Column::with_children(vec![
                            icon_button_tooltip_opt(
                                swords_button,
                                *mainhand == Some(STARTER_SWORDS),
                                Some(Message::Tool((Some(STARTER_SWORDS), None))),
                                imgs.swords,
                                "common-weapons-shortswords",
                            )
                            .into(),
                        ]),
                        ClassKind::Barbarian => Column::with_children(vec![
                            Row::with_children(vec![
                                icon_button_tooltip_opt(
                                    axe_button,
                                    *mainhand == Some(STARTER_AXE),
                                    Some(Message::Tool((Some(STARTER_AXE), None))),
                                    imgs.axe,
                                    "common-weapons-axe",
                                )
                                .into(),
                                icon_button_tooltip_opt(
                                    hammer_button,
                                    *mainhand == Some(STARTER_HAMMER),
                                    Some(Message::Tool((Some(STARTER_HAMMER), None))),
                                    imgs.hammer,
                                    "common-weapons-hammer",
                                )
                                .into(),
                            ])
                            .spacing(1)
                            .into(),
                        ]),
                    }
                    .spacing(1);

                    (tool, species, body_type, class_section)
                };

                const SLIDER_TEXT_SIZE: u16 = 20;
                const SLIDER_CURSOR_SIZE: (u16, u16) = (9, 21);
                const SLIDER_BAR_HEIGHT: u16 = 9;
                const SLIDER_BAR_PAD: u16 = 5;
                // Height of interactable area
                const SLIDER_HEIGHT: u16 = 30;

                fn starter_slider<'a>(
                    text: String,
                    size: u16,
                    state: &'a mut slider::State,
                    max: u32,
                    selected_val: u32,
                    on_change: impl 'static + Fn(u32) -> Message,
                    imgs: &Imgs,
                ) -> Element<'a, Message> {
                    Column::with_children(vec![
                        Text::new(text).size(size).into(),
                        Slider::new(state, 0..=max, selected_val, on_change)
                            .height(SLIDER_HEIGHT)
                            .style(style::slider::Style::images(
                                imgs.slider_indicator,
                                imgs.slider_range,
                                SLIDER_BAR_PAD,
                                SLIDER_CURSOR_SIZE,
                                SLIDER_BAR_HEIGHT,
                            ))
                            .into(),
                    ])
                    .align_items(Align::Center)
                    .into()
                }
                fn char_slider<'a>(
                    text: String,
                    state: &'a mut slider::State,
                    max: u8,
                    selected_val: u8,
                    on_change: impl 'static + Fn(u8) -> Message,
                    (fonts, imgs): (&Fonts, &Imgs),
                ) -> Element<'a, Message> {
                    Column::with_children(vec![
                        Text::new(text)
                            .size(fonts.cyri.scale(SLIDER_TEXT_SIZE))
                            .into(),
                        Slider::new(state, 0..=max, selected_val, on_change)
                            .height(SLIDER_HEIGHT)
                            .style(style::slider::Style::images(
                                imgs.slider_indicator,
                                imgs.slider_range,
                                SLIDER_BAR_PAD,
                                SLIDER_CURSOR_SIZE,
                                SLIDER_BAR_HEIGHT,
                            ))
                            .into(),
                    ])
                    .align_items(Align::Center)
                    .into()
                }
                fn char_slider_greyable<'a>(
                    active: bool,
                    text: String,
                    state: &'a mut slider::State,
                    max: u8,
                    selected_val: u8,
                    on_change: impl 'static + Fn(u8) -> Message,
                    (fonts, imgs): (&Fonts, &Imgs),
                ) -> Element<'a, Message> {
                    if active {
                        char_slider(text, state, max, selected_val, on_change, (fonts, imgs))
                    } else {
                        Column::with_children(vec![
                            Text::new(text)
                                .size(fonts.cyri.scale(SLIDER_TEXT_SIZE))
                                .color(DISABLED_TEXT_COLOR)
                                .into(),
                            // "Disabled" slider
                            // TODO: add iced support for disabled sliders (like buttons)
                            Slider::new(state, 0..=max.into(), selected_val.into(), |_| {
                                Message::DoNothing
                            })
                            .height(SLIDER_HEIGHT)
                            .style(style::slider::Style {
                                cursor: style::slider::Cursor::Color(Rgba::zero()),
                                bar: style::slider::Bar::Image(
                                    imgs.slider_range,
                                    Rgba::from_translucent(255, 51),
                                    SLIDER_BAR_PAD,
                                ),
                                labels: false,
                                ..Default::default()
                            })
                            .into(),
                        ])
                        .align_items(Align::Center)
                        .into()
                    }
                }

                let slider_options = Column::with_children(vec![
                    char_slider(
                        i18n.get_msg("char_selection-hair_style").into_owned(),
                        &mut sliders.hair_style,
                        body.species.num_hair_styles(body.body_type) - 1,
                        body.hair_style,
                        Message::HairStyle,
                        (fonts, imgs),
                    ),
                    char_slider(
                        i18n.get_msg("char_selection-hair_color").into_owned(),
                        &mut sliders.hair_color,
                        body.species.num_hair_colors() - 1,
                        body.hair_color,
                        Message::HairColor,
                        (fonts, imgs),
                    ),
                    char_slider(
                        i18n.get_msg("char_selection-skin").into_owned(),
                        &mut sliders.skin,
                        body.species.num_skin_colors() - 1,
                        body.skin,
                        Message::Skin,
                        (fonts, imgs),
                    ),
                    char_slider(
                        i18n.get_msg("char_selection-eyeshape").into_owned(),
                        &mut sliders.eyes,
                        body.species.num_eyes(body.body_type) - 1,
                        body.eyes,
                        Message::Eyes,
                        (fonts, imgs),
                    ),
                    char_slider(
                        i18n.get_msg("char_selection-eye_color").into_owned(),
                        &mut sliders.eye_color,
                        body.species.num_eye_colors() - 1,
                        body.eye_color,
                        Message::EyeColor,
                        (fonts, imgs),
                    ),
                    char_slider_greyable(
                        body.species.num_accessories(body.body_type) > 1,
                        i18n.get_msg("char_selection-accessories").into_owned(),
                        &mut sliders.accessory,
                        body.species.num_accessories(body.body_type) - 1,
                        body.accessory,
                        Message::Accessory,
                        (fonts, imgs),
                    ),
                    char_slider_greyable(
                        body.species.num_beards(body.body_type) > 1,
                        i18n.get_msg("char_selection-beard").into_owned(),
                        &mut sliders.beard,
                        body.species.num_beards(body.body_type) - 1,
                        body.beard,
                        Message::Beard,
                        (fonts, imgs),
                    ),
                ])
                .max_width(200)
                .padding(5);

                // BL-33: starting moral-alignment picker (its own wizard step).
                // Two rows — Good/Neutral/Evil and Lawful/Neutral/Chaotic. Each
                // button uses a FIXED-size centred label (not FillText) so every
                // word renders at the same size, wrapped in the shared button
                // image (uniform width, no distortion). Selection = gold text.
                // The pick is only a starting point; deeds drift it in-game.
                let [moral_good_btn, moral_neutral_btn, moral_evil_btn] = ethos_moral_buttons;
                let [order_lawful_btn, order_neutral_btn, order_chaotic_btn] = ethos_order_buttons;
                const ETHOS_TEXT: u16 = 20;
                const ETHOS_ROW_H: u16 = 40;
                const ETHOS_GAP: u16 = 8;
                const ETHOS_ROW_W: u32 = 360;
                let ethos_text = fonts.cyri.scale(ETHOS_TEXT);
                let ethos_section = Column::with_children(vec![
                    Row::with_children(vec![
                        fixed_label_button(
                            moral_good_btn,
                            i18n.get_msg("char_selection-ethos_good").into_owned(),
                            ethos_text,
                            ethos.moral() == Moral::Good,
                            button_style,
                            Message::EthosMoral(Moral::Good),
                        ),
                        fixed_label_button(
                            moral_neutral_btn,
                            i18n.get_msg("char_selection-ethos_neutral").into_owned(),
                            ethos_text,
                            ethos.moral() == Moral::Neutral,
                            button_style,
                            Message::EthosMoral(Moral::Neutral),
                        ),
                        fixed_label_button(
                            moral_evil_btn,
                            i18n.get_msg("char_selection-ethos_evil").into_owned(),
                            ethos_text,
                            ethos.moral() == Moral::Evil,
                            button_style,
                            Message::EthosMoral(Moral::Evil),
                        ),
                    ])
                    .height(Length::Units(ETHOS_ROW_H))
                    .spacing(ETHOS_GAP)
                    .into(),
                    Row::with_children(vec![
                        fixed_label_button(
                            order_lawful_btn,
                            i18n.get_msg("char_selection-ethos_lawful").into_owned(),
                            ethos_text,
                            ethos.order() == Order::Lawful,
                            button_style,
                            Message::EthosOrder(Order::Lawful),
                        ),
                        fixed_label_button(
                            order_neutral_btn,
                            i18n.get_msg("char_selection-ethos_neutral").into_owned(),
                            ethos_text,
                            ethos.order() == Order::Neutral,
                            button_style,
                            Message::EthosOrder(Order::Neutral),
                        ),
                        fixed_label_button(
                            order_chaotic_btn,
                            i18n.get_msg("char_selection-ethos_chaotic").into_owned(),
                            ethos_text,
                            ethos.order() == Order::Chaotic,
                            button_style,
                            Message::EthosOrder(Order::Chaotic),
                        ),
                    ])
                    .height(Length::Units(ETHOS_ROW_H))
                    .spacing(ETHOS_GAP)
                    .into(),
                ])
                .align_items(Align::Center)
                .spacing(ETHOS_GAP)
                // Cap the width so the 3 equal-thirds buttons stay a readable
                // size and never overflow the panel (centred by the parent).
                .width(Length::Fill)
                .max_width(ETHOS_ROW_W);

                // BL-31 UI-fixes (spec §4): the Background step — a 2-column
                // grid of every `BackgroundKind`, radio-select (spec §1: the
                // wizard always has exactly one background selected; clicking
                // any entry always selects it, never toggles off). Mirrors the
                // `characters`/`characters_scroll` Vec<button::State> pattern
                // (character select list) rather than the Ethos step's fixed
                // 3-button grid, since dozens of backgrounds don't fit a
                // fixed layout. Per-background flavor text / category headers
                // are a future content pass (spec §5); this step lists names
                // only, via `BackgroundKind::display_name()` (a title-cased
                // stand-in for the real i18n titles that pass will author).
                const BACKGROUND_ROW_H: u16 = 40;
                // Long display names get a smaller font fraction so they
                // don't clip at the grid's cell width. Post-curation (BL-31,
                // 2026-07-02) the 24-background V1 catalogue's longest names
                // are "Archaeologist"/"Bounty Hunter" (13 chars) and
                // "Investigator" (12 chars) — none exceed this threshold, so
                // the fallback branch is currently unreachable but kept as a
                // defensive guard for future longer names.
                const BACKGROUND_LONG_NAME_LEN: usize = 14;
                let background_section = {
                    background_buttons.resize_with(BackgroundKind::ALL.len(), Default::default);

                    // BL-31 V2 (spec §1): the grid renders alphabetically by
                    // `display_name()` (A top-left → Z bottom-right), but
                    // `BackgroundKind::ALL`'s declaration order (lore
                    // category) stays untouched — persistence/`keyword()`
                    // round-trips and tests key off the enum, not this
                    // render-only copy. `background_buttons`' states are
                    // transient (hover/press only, no per-kind identity), so
                    // zipping them against this sorted order is safe.
                    let mut ordered: Vec<BackgroundKind> = BackgroundKind::ALL.to_vec();
                    ordered.sort_by_key(|kind| kind.display_name());

                    let buttons = ordered
                        .into_iter()
                        .zip(background_buttons.iter_mut())
                        .map(|(kind, state)| {
                            let selected = background.0 == Some(kind);
                            let label = kind.display_name();
                            let fill_fraction = if label.len() > BACKGROUND_LONG_NAME_LEN {
                                FILL_FRAC_TWO
                            } else {
                                FILL_FRAC_ONE
                            };
                            let el = neat_button(
                                state,
                                label,
                                fill_fraction,
                                if selected {
                                    style::button::Style::new(imgs.button)
                                        .hover_image(imgs.button_hover)
                                        .press_image(imgs.button_press)
                                        .text_color(Color::from_rgb(0.93, 0.78, 0.28))
                                } else {
                                    button_style
                                },
                                // Clicking always selects (spec §1): the
                                // Background step is a single-select radio
                                // group and never renders with nothing
                                // selected, so re-clicking the current entry
                                // is a no-op rather than clearing it.
                                Some(Message::Background(Some(kind))),
                            );
                            Container::new(el)
                                .width(Length::Fill)
                                .height(Length::Units(BACKGROUND_ROW_H))
                                .into()
                        })
                        .collect::<Vec<Element<Message>>>();

                    // Chunk into rows of two so the grid stays agnostic to the
                    // total variant count (spec §0.1) — trimming the enum
                    // later just yields fewer rows, no code change needed.
                    // `Element` isn't `Clone`, so pair up by draining the
                    // owned `Vec` two at a time instead of `.chunks()`.
                    let mut buttons = buttons.into_iter();
                    let mut rows: Vec<Element<Message>> = Vec::new();
                    loop {
                        let Some(first) = buttons.next() else {
                            break;
                        };
                        let mut row_children = vec![first];
                        if let Some(second) = buttons.next() {
                            row_children.push(second);
                        }
                        rows.push(
                            Row::with_children(row_children)
                                .spacing(6)
                                .width(Length::Fill)
                                .into(),
                        );
                    }

                    let grid = Column::with_children(rows).spacing(4).width(Length::Fill);

                    Container::new(
                        Scrollable::new(background_scroll)
                            .push(grid)
                            .padding(6)
                            .scrollbar_width(5)
                            .scroller_width(5)
                            .width(Length::Fill)
                            .style(style::scrollable::Style {
                                track: None,
                                scroller: style::scrollable::Scroller::Color(UI_MAIN),
                            }),
                    )
                    // Roughly double the old single-column height (260) so
                    // the 2-column grid needs little to no scrolling
                    // (spec §4.2).
                    .height(Length::Units(520))
                    .width(Length::Fill)
                };

                let hardcore_checkbox = if character_id.is_some() {
                    Row::new()
                } else {
                    Row::with_children(vec![
                        Checkbox::new(
                            *hardcore_enabled,
                            i18n.get_msg("char_selection-hardcore"),
                            Message::HardcoreEnabled,
                        )
                        .size(40)
                        .spacing(10)
                        .text_size(30)
                        .style(style::checkbox::Style::new(
                            imgs.icon_border,
                            self.imgs.hardcore,
                        ))
                        .with_tooltip(tooltip_manager, move || {
                            let tooltip_text = i18n.get_msg("char_selection-hardcore_tooltip");
                            tooltip::text(&tooltip_text, tooltip_style)
                        })
                        .into(),
                    ])
                };

                // BL-33: review summary for the final wizard step — a recap of
                // everything chosen (name, race, class, alignment).
                let summary = {
                    let moral_key = match ethos.moral() {
                        Moral::Good => "char_selection-ethos_good",
                        Moral::Neutral => "char_selection-ethos_neutral",
                        Moral::Evil => "char_selection-ethos_evil",
                    };
                    let order_key = match ethos.order() {
                        Order::Lawful => "char_selection-ethos_lawful",
                        Order::Neutral => "char_selection-ethos_neutral",
                        Order::Chaotic => "char_selection-ethos_chaotic",
                    };
                    let alignment_str =
                        if ethos.moral() == Moral::Neutral && ethos.order() == Order::Neutral {
                            i18n.get_msg("char_selection-ethos_true_neutral")
                                .into_owned()
                        } else {
                            format!("{} {}", i18n.get_msg(order_key), i18n.get_msg(moral_key))
                        };
                    // Creation only offers these four classes (the class step).
                    let class_key = match class {
                        ClassKind::Mage => "char_selection-class_mage",
                        ClassKind::Cleric => "char_selection-class_cleric",
                        ClassKind::Rogue => "char_selection-class_rogue",
                        _ => "char_selection-class_warrior",
                    };
                    let class_name = i18n.get_msg(class_key).into_owned();
                    // A tidy key/value row: a muted, right-aligned label in a
                    // fixed-width column so the values line up, then the value.
                    let kv = |label_key: &str, value: String| -> Element<Message> {
                        Row::with_children(vec![
                            Text::new(i18n.get_msg(label_key).into_owned())
                                .size(fonts.cyri.scale(20))
                                .width(Length::Units(110))
                                .horizontal_alignment(HorizontalAlignment::Right)
                                .color(Color::from_rgb(0.65, 0.65, 0.65))
                                .into(),
                            Text::new(value)
                                .size(fonts.cyri.scale(20))
                                .color(TEXT_COLOR)
                                .into(),
                        ])
                        .spacing(14)
                        .align_items(Align::Center)
                        .into()
                    };
                    Column::with_children(vec![
                        kv("char_selection-summary_label_name", name.clone()),
                        kv(
                            "char_selection-summary_label_race",
                            // Use the localized species name (renamed via i18n,
                            // e.g. Danari→Gnome) rather than the Debug enum name.
                            i18n.get_msg(match body.species {
                                humanoid::Species::Danari => "common-species-danari",
                                humanoid::Species::Dwarf => "common-species-dwarf",
                                humanoid::Species::Elf => "common-species-elf",
                                humanoid::Species::Human => "common-species-human",
                                humanoid::Species::Orc => "common-species-orc",
                                humanoid::Species::Draugr => "common-species-draugr",
                            })
                            .into_owned(),
                        ),
                        kv("char_selection-summary_label_class", class_name),
                        kv("char_selection-summary_label_alignment", alignment_str),
                        kv(
                            "char_selection-summary_label_background",
                            // BL-31 task BG2b.2 recap row: background name, or
                            // "Uncommitted" (P0 §Q1) if none was chosen.
                            match &background.0 {
                                Some(kind) => kind.display_name(),
                                None => i18n
                                    .get_msg("char_selection-background_uncommitted")
                                    .into_owned(),
                            },
                        ),
                    ])
                    .align_items(Align::Start)
                    .spacing(10)
                };

                const CHAR_DICE_SIZE: u16 = 50;
                let rand_character = Button::new(
                    rand_character_button,
                    Space::new(Length::Units(CHAR_DICE_SIZE), Length::Units(CHAR_DICE_SIZE)),
                )
                .style(
                    style::button::Style::new(imgs.dice)
                        .hover_image(imgs.dice_hover)
                        .press_image(imgs.dice_press),
                )
                .on_press(Message::RandomizeCharacter)
                .with_tooltip(tooltip_manager, move || {
                    let tooltip_text = i18n.get_msg("common-rand_appearance");
                    tooltip::text(&tooltip_text, tooltip_style)
                });

                let left_column_content: Vec<Element<Message>> = if character_id.is_some() {
                    // Edit mode keeps the single combined screen.
                    vec![
                        body_type.into(),
                        class_section.into(),
                        tool.into(),
                        species.into(),
                        slider_options.into(),
                        hardcore_checkbox.into(),
                        rand_character.into(),
                    ]
                } else {
                    // Creation mode: the wizard renders one step at a time. The
                    // title for the current step replaces the per-section titles.
                    let step_title_key = match step {
                        CreationStep::Body => "char_selection-step_body",
                        CreationStep::Appearance => "char_selection-step_appearance",
                        CreationStep::Class => "char_selection-step_class",
                        CreationStep::Alignment => "char_selection-step_alignment",
                        CreationStep::Background => "char_selection-step_background",
                        CreationStep::Finish => "char_selection-step_finish",
                    };
                    let step_title: Element<Message> =
                        Text::new(i18n.get_msg(step_title_key).into_owned())
                            .size(fonts.cyri.scale(26))
                            .into();
                    match step {
                        // Sex and race get their own labelled sections with
                        // breathing room so it's clear which group is which.
                        CreationStep::Body => vec![
                            Text::new(i18n.get_msg("char_selection-sex").into_owned())
                                .size(fonts.cyri.scale(18))
                                .into(),
                            body_type.into(),
                            Space::new(Length::Fill, Length::Units(12)).into(),
                            step_title,
                            species.into(),
                        ],
                        CreationStep::Appearance => {
                            vec![step_title, slider_options.into(), rand_character.into()]
                        },
                        CreationStep::Class => {
                            vec![step_title, class_section.into(), tool.into()]
                        },
                        CreationStep::Alignment => vec![step_title, ethos_section.into()],
                        CreationStep::Background => {
                            vec![step_title, background_section.into()]
                        },
                        CreationStep::Finish => vec![
                            step_title,
                            Space::new(Length::Fill, Length::Units(14)).into(),
                            summary.into(),
                            Space::new(Length::Fill, Length::Units(22)).into(),
                            hardcore_checkbox.into(),
                        ],
                    }
                };

                // The start-zone map panel renders only on the Finish step (and
                // never in edit mode).
                let show_map = character_id.is_none() && step == CreationStep::Finish;
                // BL-31 UI-fixes (spec §4.3): the companion detail panel for
                // the currently-selected background renders only on the
                // Background step (and never in edit mode), mirroring
                // `show_map`'s gating pattern.
                let show_background_detail =
                    character_id.is_none() && step == CreationStep::Background;
                let right_column_content = if show_map {
                    let map_sz = Vec2::new(500, 500);
                    let map_img = Image::new(self.map_img)
                        .height(Length::Units(map_sz.x))
                        .width(Length::Units(map_sz.y));
                    /* .stroke(Stroke {
                        color: Color::WHITE,
                        width: 1.0,
                    }) */
                    //TODO: Add text-outline here whenever we updated iced to a version supporting
                    // this

                    let map = if let Some(info) = self
                        .possible_starting_sites
                        .get(start_site_idx.unwrap_or_default())
                    {
                        let site_name = Text::new(
                            self.possible_starting_sites[start_site_idx.unwrap_or_default()]
                                .label
                                .as_ref()
                                .map(|name| i18n.get_content(name))
                                .unwrap_or_else(|| "Unknown".to_string()),
                        )
                        .horizontal_alignment(HorizontalAlignment::Left)
                        .color(Color::from_rgb(131.0, 102.0, 0.0));
                        let pos_frac = info
                            .wpos
                            .map2(self.world_sz * TerrainChunkSize::RECT_SIZE, |e, sz| {
                                e / sz as f32
                            });
                        let point = Vec2::new(pos_frac.x, 1.0 - pos_frac.y)
                            .map2(map_sz, |e, sz| e * sz as f32 - 12.0);
                        let marker_img = Image::new(imgs.town_marker)
                            .height(Length::Units(27))
                            .width(Length::Units(16));
                        let marker_content: Column<Message, IcedRenderer> = Column::new()
                            .spacing(2)
                            .push(site_name)
                            .push(marker_img)
                            .align_items(Align::Center);

                        Overlay::new(
                            Container::new(marker_content)
                                .width(Length::Fill)
                                .height(Length::Fill)
                                .center_x()
                                .center_y(),
                            map_img,
                        )
                        .over_position(iced::Point::new(point.x, point.y - 34.0))
                        .into()
                    } else {
                        map_img.into()
                    };

                    if self.possible_starting_sites.is_empty() {
                        vec![map]
                    } else {
                        let selected = start_site_idx.get_or_insert_with(|| {
                            rng().random_range(0..self.possible_starting_sites.len())
                        });

                        let site_slider = starter_slider(
                            i18n.get_msg("char_selection-starting_site").into_owned(),
                            30,
                            &mut sliders.starting_site,
                            self.possible_starting_sites.len() as u32 - 1,
                            *selected as u32,
                            |x| Message::StartingSite(x as usize),
                            imgs,
                        );
                        let site_buttons = Row::with_children(vec![
                            neat_button(
                                prev_starting_site_button,
                                i18n.get_msg("char_selection-starting_site_prev")
                                    .into_owned(),
                                FILL_FRAC_ONE,
                                button_style,
                                Some(Message::PrevStartingSite),
                            ),
                            neat_button(
                                next_starting_site_button,
                                i18n.get_msg("char_selection-starting_site_next")
                                    .into_owned(),
                                FILL_FRAC_ONE,
                                button_style,
                                Some(Message::NextStartingSite),
                            ),
                        ])
                        .max_height(60)
                        .padding(15)
                        .into();
                        // Todo: use this to change the site icon if we use different starting site
                        // types
                        /* let site_kind = Text::new(i18n
                            .get_msg_ctx("char_selection-starting_site_kind", &i18n::fluent_args! {
                                "kind" => match self.possible_starting_sites[*start_site_idx].kind {
                                    SiteKind::Town => i18n.get_msg("hud-map-town").into_owned(),
                                    SiteKind::Castle => i18n.get_msg("hud-map-castle").into_owned(),
                                    SiteKind::Bridge => i18n.get_msg("hud-map-bridge").into_owned(),
                                    _ => "Unknown".to_string(),
                                },
                            })
                            .into_owned())
                        .size(fonts.cyri.scale(SLIDER_TEXT_SIZE))
                        .into(); */

                        vec![site_slider, map, site_buttons]
                    }
                } else if show_background_detail {
                    background_detail_panel(background.0, i18n, fonts)
                } else {
                    // If we're editing an existing character, don't display the world column
                    Vec::new()
                };

                // BL-31 UI-fixes (spec §4.2): the Background step's 2-column
                // grid needs a wider left column than the other steps'
                // single-column content, so `column_left` takes the target
                // width instead of hardcoding it.
                let left_column_width =
                    if character_id.is_none() && step == CreationStep::Background {
                        480
                    } else {
                        320
                    };
                let column_left = |column_content, scroll, width: u16| {
                    let column = Container::new(
                        Scrollable::new(scroll)
                            .push(
                                Column::with_children(column_content)
                                    .align_items(Align::Center)
                                    .width(Length::Fill)
                                    .spacing(5)
                                    .padding(5),
                            )
                            .padding(5)
                            .width(Length::Fill)
                            .align_items(Align::Center)
                            .style(style::scrollable::Style {
                                track: None,
                                scroller: style::scrollable::Scroller::Color(UI_MAIN),
                            }),
                    )
                    .width(Length::Units(width)) // TODO: see if we can get iced to work with settings below
                    // .max_width(360)
                    // .width(Length::Fill)
                    .height(Length::Fill);

                    Column::with_children(vec![
                        Container::new(column)
                            .style(style::container::Style::color(Rgba::from_translucent(
                                0,
                                BANNER_ALPHA,
                            )))
                            .width(Length::Units(width))
                            .center_x()
                            .into(),
                        Image::new(imgs.frame_bottom)
                            .height(Length::Units(40))
                            .width(Length::Units(width))
                            .color(Rgba::from_translucent(0, BANNER_ALPHA))
                            .into(),
                    ])
                    .height(Length::Fill)
                };
                // BL-31 V2 (spec §2): the Background step's detail panel now
                // carries more content (real Detalle paragraph + 2nd passive
                // + Sociedad line) than the map panel that shares this
                // closure, and a bare `Length::Fill` inner container was
                // fighting the fixed 40px `frame_bottom` image stacked below
                // it in the outer `Length::Fill` column — the frame ate into
                // the content instead of sitting flush beneath it, clipping
                // the panel's last sections. Giving the inner container an
                // explicit floor (mirroring the left grid's
                // `Length::Units(520)`) lets it claim real space above the
                // frame; the `Scrollable` still catches any overflow. The map
                // panel (Finish step) keeps the original `Length::Fill` since
                // it isn't affected by this clipping.
                const BACKGROUND_DETAIL_CONTENT_HEIGHT: u16 = 620;
                let column_right = |column_content, scroll, content_height| {
                    let column = Container::new(
                        Scrollable::new(scroll)
                            .push(
                                Column::with_children(column_content)
                                    .align_items(Align::Center)
                                    .width(Length::Fill)
                                    .spacing(5)
                                    .padding(5),
                            )
                            .padding(5)
                            .width(Length::Fill)
                            .align_items(Align::Center)
                            .style(style::scrollable::Style {
                                track: None,
                                scroller: style::scrollable::Scroller::Color(UI_MAIN),
                            }),
                    )
                    .width(Length::Units(520)) // TODO: see if we can get iced to work with settings below
                    // .max_width(360)
                    // .width(Length::Fill)
                    .height(content_height);
                    // Only the Finish step's map panel and the Background
                    // step's detail panel (both creation-mode only) show the
                    // framed right column; everything else keeps it empty/bare.
                    if show_map || show_background_detail {
                        Column::with_children(vec![
                            Container::new(column)
                                .style(style::container::Style::color(Rgba::from_translucent(
                                    0,
                                    BANNER_ALPHA,
                                )))
                                .width(Length::Units(520))
                                .center_x()
                                .into(),
                            Image::new(imgs.frame_bottom)
                                .height(Length::Units(40))
                                .width(Length::Units(520))
                                .color(Rgba::from_translucent(0, BANNER_ALPHA))
                                .into(),
                        ])
                        .height(Length::Fill)
                    } else {
                        Column::with_children(vec![Container::new(column).into()])
                    }
                };

                let mouse_area =
                    MouseDetector::new(&mut self.mouse_detector, Length::Fill, Length::Fill);

                let top = Row::with_children(vec![
                    column_left(left_column_content, left_scroll, left_column_width).into(),
                    Column::with_children(
                        if let Some(warning_container) = warning_container.take() {
                            vec![warning_container.into(), mouse_area.into()]
                        } else {
                            vec![mouse_area.into()]
                        },
                    )
                    .width(Length::Fill)
                    .height(Length::Fill)
                    .into(),
                    column_right(
                        right_column_content,
                        right_scroll,
                        if show_background_detail {
                            Length::Units(BACKGROUND_DETAIL_CONTENT_HEIGHT)
                        } else {
                            Length::Fill
                        },
                    )
                    .width(Length::Units(520))
                    .into(),
                ])
                .padding(10)
                .width(Length::Fill)
                .height(Length::Fill);

                let back = neat_button(
                    back_button,
                    i18n.get_msg("common-back").into_owned(),
                    FILL_FRAC_ONE,
                    button_style,
                    Some(Message::Back),
                );

                const NAME_DICE_SIZE: u16 = 35;
                let rand_name = Button::new(
                    rand_name_button,
                    Space::new(Length::Units(NAME_DICE_SIZE), Length::Units(NAME_DICE_SIZE)),
                )
                .style(
                    style::button::Style::new(imgs.dice)
                        .hover_image(imgs.dice_hover)
                        .press_image(imgs.dice_press),
                )
                .on_press(Message::RandomizeName)
                .with_tooltip(tooltip_manager, move || {
                    let tooltip_text = i18n.get_msg("common-rand_name");
                    tooltip::text(&tooltip_text, tooltip_style)
                });

                let confirm_msg = if let Some(character_id) = character_id {
                    Message::ConfirmEdit(*character_id)
                } else {
                    Message::CreateCharacter
                };

                let name_input = BackgroundContainer::new(
                    Image::new(imgs.name_input)
                        .height(Length::Units(40))
                        .fix_aspect_ratio(),
                    TextInput::new(
                        name_input,
                        &i18n.get_msg("character_window-character_name"),
                        name,
                        Message::Name,
                    )
                    .size(25)
                    .on_submit(confirm_msg.clone()),
                )
                .padding(Padding::new().horizontal(7).top(5));

                let bottom_center = Container::new(
                    Row::with_children(vec![
                        rand_name.into(),
                        name_input.into(),
                        Space::new(Length::Units(NAME_DICE_SIZE), Length::Units(NAME_DICE_SIZE))
                            .into(),
                    ])
                    .align_items(Align::Center)
                    .spacing(5)
                    .padding(16),
                )
                .style(style::container::Style::color(Rgba::new(0, 0, 0, 100)));

                let create = neat_button(
                    create_button,
                    i18n.get_msg(if character_id.is_some() {
                        "common-confirm"
                    } else {
                        "common-create"
                    }),
                    FILL_FRAC_ONE,
                    button_style,
                    (!name.is_empty()).then_some(confirm_msg),
                );

                let create: Element<Message> = if name.is_empty() {
                    create
                        .with_tooltip(tooltip_manager, move || {
                            let tooltip_text = i18n.get_msg("char_selection-create_info_name");
                            tooltip::text(&tooltip_text, tooltip_style)
                        })
                        .into()
                } else {
                    create
                };

                // In creation mode the Crear button only appears on the final
                // step; otherwise the wizard's "Next" advances the step. In edit
                // mode the Confirm button is always shown.
                let show_create = character_id.is_some() || step == CreationStep::Finish;
                let create_cell: Element<Message> = if show_create {
                    Container::new(create)
                        .width(Length::Fill)
                        .height(Length::Units(SMALL_BUTTON_HEIGHT))
                        .align_x(Align::End)
                        .into()
                } else {
                    // Reserve the slot so the name input stays centred.
                    Container::new(Space::new(Length::Fill, Length::Shrink))
                        .width(Length::Fill)
                        .height(Length::Units(SMALL_BUTTON_HEIGHT))
                        .into()
                };

                let bottom = Row::with_children(vec![
                    Container::new(back)
                        .width(Length::Fill)
                        .height(Length::Units(SMALL_BUTTON_HEIGHT))
                        .into(),
                    Container::new(bottom_center)
                        .width(Length::Fill)
                        .center_x()
                        .into(),
                    create_cell,
                ])
                .align_items(Align::End);

                // Wizard navigation bar (creation mode only).
                let nav_bar: Option<Element<Message>> = if character_id.is_none() {
                    let prev = neat_button(
                        wizard_back_button,
                        i18n.get_msg("char_selection-wizard_back").into_owned(),
                        FILL_FRAC_ONE,
                        button_style,
                        // Disabled on the first step.
                        (step != CreationStep::Body).then_some(Message::WizardBack),
                    );
                    let progress: Element<Message> = Text::new(
                        i18n.get_msg_ctx("char_selection-wizard_step", &i18n::fluent_args! {
                            "step" => step.index(),
                        })
                        .into_owned(),
                    )
                    .size(fonts.cyri.scale(20))
                    .horizontal_alignment(HorizontalAlignment::Center)
                    .into();
                    // The "Next" slot becomes empty on the final step (the Crear
                    // button takes over in the bottom row).
                    let next_cell: Element<Message> = if step == CreationStep::Finish {
                        Space::new(Length::Fill, Length::Shrink).into()
                    } else {
                        neat_button(
                            wizard_next_button,
                            i18n.get_msg("char_selection-wizard_next").into_owned(),
                            FILL_FRAC_ONE,
                            button_style,
                            Some(Message::WizardNext),
                        )
                    };
                    Some(
                        Row::with_children(vec![
                            Container::new(prev).width(Length::Fill).center_x().into(),
                            Container::new(progress)
                                .width(Length::Fill)
                                .center_x()
                                .into(),
                            Container::new(next_cell)
                                .width(Length::Fill)
                                .center_x()
                                .into(),
                        ])
                        .height(Length::Units(28))
                        .align_items(Align::Center)
                        .padding(5)
                        .into(),
                    )
                } else {
                    None
                };

                let mut bottom_children: Vec<Element<Message>> = vec![top.into()];
                if let Some(nav_bar) = nav_bar {
                    bottom_children.push(nav_bar);
                }
                bottom_children.push(bottom.into());

                Column::with_children(bottom_children)
                    .width(Length::Fill)
                    .height(Length::Fill)
                    .padding(5)
                    .into()
            },
        };

        let children = if let Some(warning_container) = warning_container {
            vec![top_text.into(), warning_container.into(), content]
        } else {
            vec![top_text.into(), content]
        };

        Container::new(
            Column::with_children(children)
                .spacing(3)
                .width(Length::Fill)
                .height(Length::Fill),
        )
        .padding(3)
        .into()
    }

    fn update(&mut self, message: Message, events: &mut Vec<Event>, characters: &[CharacterItem]) {
        match message {
            Message::Back => {
                if matches!(&self.mode, Mode::CreateOrEdit { .. }) {
                    self.mode = Mode::select(None);
                }
            },
            Message::Logout => {
                events.push(Event::Logout);
            },
            Message::ShowRules => {
                events.push(Event::ShowRules);
            },
            Message::ConfirmDeletion => {
                if let Mode::Select { info_content, .. } = &mut self.mode
                    && let Some(InfoContent::Deletion(idx)) = info_content
                {
                    if let Some(id) = characters.get(*idx).and_then(|i| i.character.id) {
                        events.push(Event::DeleteCharacter(id));
                        // Deselect if the selected character was deleted
                        if Some(id) == self.selected {
                            self.selected = None;
                            events.push(Event::SelectCharacter(None));
                        }
                    }
                    *info_content = None;
                }
            },
            Message::CancelDeletion => {
                if let Mode::Select { info_content, .. } = &mut self.mode
                    && let Some(InfoContent::Deletion(_)) = info_content
                {
                    *info_content = None;
                }
            },
            Message::ClearCharacterListError => {
                events.push(Event::ClearCharacterListError);
            },
            Message::DoNothing => {},
            _ if matches!(self.mode, Mode::Select {
                info_content: Some(_),
                ..
            }) =>
            {
                // Don't allow use of the UI on the select screen to deal with
                // things other than the event currently being
                // procesed; all the select screen events after this
                // modify the info content or selection, except for Spectate
                // which currently causes us to exit the
                // character select state.
            },
            Message::EnterWorld => {
                if let (Mode::Select { info_content, .. }, Some(selected)) =
                    (&mut self.mode, self.selected)
                {
                    events.push(Event::Play(selected));
                    *info_content = Some(InfoContent::JoiningCharacter);
                }
            },
            Message::Spectate => {
                if matches!(self.mode, Mode::Select { .. }) {
                    events.push(Event::Spectate);
                    // FIXME: Enter JoiningCharacter when we have a proper error
                    // event for spectating.
                }
            },
            Message::Select(id) => {
                if let Mode::Select { .. } = &mut self.mode {
                    self.selected = Some(id);
                    events.push(Event::SelectCharacter(Some(id)))
                }
            },
            Message::Delete(idx) => {
                if let Mode::Select { info_content, .. } = &mut self.mode {
                    *info_content = Some(InfoContent::Deletion(idx));
                }
            },
            Message::Edit(idx) => {
                if matches!(&self.mode, Mode::Select { .. })
                    && let Some(character) = characters.get(idx)
                    && let comp::Body::Humanoid(body) = character.body
                    && let Some(id) = character.character.id
                {
                    self.mode = Mode::edit(
                        character.character.alias.clone(),
                        id,
                        body,
                        &character.inventory,
                    );
                }
            },
            Message::NewCharacter => {
                if matches!(&self.mode, Mode::Select { .. }) {
                    self.mode = Mode::create(self.default_name.clone());
                }
            },
            Message::CreateCharacter => {
                if let Mode::CreateOrEdit {
                    name,
                    body,
                    hardcore_enabled,
                    mainhand,
                    offhand,
                    class,
                    ethos,
                    background,
                    start_site_idx,
                    ..
                } = &self.mode
                {
                    events.push(Event::AddCharacter {
                        alias: name.clone(),
                        mainhand: mainhand.map(String::from),
                        offhand: offhand.map(String::from),
                        body: comp::Body::Humanoid(*body),
                        hardcore: *hardcore_enabled,
                        class: *class,
                        ethos: *ethos,
                        background: *background,
                        start_site: self
                            .possible_starting_sites
                            .get(start_site_idx.unwrap_or_default())
                            .and_then(|info| info.site),
                    });
                    self.mode = Mode::select(Some(InfoContent::CreatingCharacter));
                }
            },
            Message::ConfirmEdit(character_id) => {
                if let Mode::CreateOrEdit { name, body, .. } = &self.mode {
                    events.push(Event::EditCharacter {
                        alias: name.clone(),
                        character_id,
                        body: comp::Body::Humanoid(*body),
                    });
                    self.mode = Mode::select(Some(InfoContent::EditingCharacter));
                }
            },
            Message::Name(value) => {
                if let Mode::CreateOrEdit { name, .. } = &mut self.mode {
                    *name = value.chars().take(MAX_NAME_LENGTH).collect();
                }
            },
            Message::BodyType(value) => {
                if let Mode::CreateOrEdit { body, .. } = &mut self.mode {
                    body.body_type = value;
                    body.validate();
                }
            },
            Message::Species(value) => {
                if let Mode::CreateOrEdit { body, .. } = &mut self.mode {
                    body.species = value;
                    body.validate();
                }
            },
            Message::Class(value) => {
                if let Mode::CreateOrEdit {
                    class,
                    mainhand,
                    offhand,
                    inventory,
                    ..
                } = &mut self.mode
                {
                    *class = value;
                    let (new_mainhand, new_offhand) = default_starter_for_class(value);
                    *mainhand = new_mainhand;
                    *offhand = new_offhand;
                    inventory.replace_loadout_item(
                        EquipSlot::ActiveMainhand,
                        mainhand.map(Item::new_from_asset_expect),
                        // Voxygen is not authoritative on inventory so we don't care if fake time
                        // is supplied
                        Time(0.0),
                    );
                    inventory.replace_loadout_item(
                        EquipSlot::ActiveOffhand,
                        offhand.map(Item::new_from_asset_expect),
                        // Voxygen is not authoritative on inventory so we don't care if fake time
                        // is supplied
                        Time(0.0),
                    );
                }
            },
            Message::EthosMoral(moral) => {
                if let Mode::CreateOrEdit { ethos, .. } = &mut self.mode {
                    *ethos = Ethos::from_box(ethos.order(), moral);
                }
            },
            Message::EthosOrder(order) => {
                if let Mode::CreateOrEdit { ethos, .. } = &mut self.mode {
                    *ethos = Ethos::from_box(order, ethos.moral());
                }
            },
            Message::Background(kind) => {
                if let Mode::CreateOrEdit { background, .. } = &mut self.mode {
                    background.0 = kind;
                }
            },
            Message::Tool(value) => {
                if let Mode::CreateOrEdit {
                    mainhand,
                    offhand,
                    inventory,
                    ..
                } = &mut self.mode
                {
                    *mainhand = value.0;
                    *offhand = value.1;
                    inventory.replace_loadout_item(
                        EquipSlot::ActiveMainhand,
                        mainhand.map(Item::new_from_asset_expect),
                        // Voxygen is not authoritative on inventory so we don't care if fake time
                        // is supplied
                        Time(0.0),
                    );
                    inventory.replace_loadout_item(
                        EquipSlot::ActiveOffhand,
                        offhand.map(Item::new_from_asset_expect),
                        // Voxygen is not authoritative on inventory so we don't care if fake time
                        // is supplied
                        Time(0.0),
                    );
                }
            },
            //Todo: Add species and body type to randomization.
            Message::RandomizeCharacter => {
                if let Mode::CreateOrEdit { body, .. } = &mut self.mode {
                    let body_type = body.body_type;
                    let species = body.species;
                    let mut rng = rand::rng();
                    body.hair_style = rng.random_range(0..species.num_hair_styles(body_type));
                    body.beard = rng.random_range(0..species.num_beards(body_type));
                    body.accessory = rng.random_range(0..species.num_accessories(body_type));
                    body.hair_color = rng.random_range(0..species.num_hair_colors());
                    body.skin = rng.random_range(0..species.num_skin_colors());
                    body.eye_color = rng.random_range(0..species.num_eye_colors());
                    body.eyes = rng.random_range(0..species.num_eyes(body_type));
                }
            },
            Message::HardcoreEnabled(checked) => {
                if let Mode::CreateOrEdit {
                    hardcore_enabled: hardcore_checkbox,
                    ..
                } = &mut self.mode
                {
                    *hardcore_checkbox = checked;
                }
            },
            Message::RandomizeName => {
                if let Mode::CreateOrEdit { name, body, .. } = &mut self.mode {
                    use common::npc;
                    *name = npc::get_npc_name(
                        npc::NpcKind::Humanoid,
                        npc::BodyType::from_body(comp::Body::Humanoid(*body)),
                    );
                }
            },
            Message::HairStyle(value) => {
                if let Mode::CreateOrEdit { body, .. } = &mut self.mode {
                    body.hair_style = value;
                    body.validate();
                }
            },
            Message::HairColor(value) => {
                if let Mode::CreateOrEdit { body, .. } = &mut self.mode {
                    body.hair_color = value;
                    body.validate();
                }
            },
            Message::Skin(value) => {
                if let Mode::CreateOrEdit { body, .. } = &mut self.mode {
                    body.skin = value;
                    body.validate();
                }
            },
            Message::Eyes(value) => {
                if let Mode::CreateOrEdit { body, .. } = &mut self.mode {
                    body.eyes = value;
                    body.validate();
                }
            },
            Message::EyeColor(value) => {
                if let Mode::CreateOrEdit { body, .. } = &mut self.mode {
                    body.eye_color = value;
                    body.validate();
                }
            },
            Message::Accessory(value) => {
                if let Mode::CreateOrEdit { body, .. } = &mut self.mode {
                    body.accessory = value;
                    body.validate();
                }
            },
            Message::Beard(value) => {
                if let Mode::CreateOrEdit { body, .. } = &mut self.mode {
                    body.beard = value;
                    body.validate();
                }
            },
            Message::StartingSite(idx) => {
                if let Mode::CreateOrEdit { start_site_idx, .. } = &mut self.mode {
                    *start_site_idx = Some(idx);
                }
            },
            Message::PrevStartingSite => {
                if let Mode::CreateOrEdit { start_site_idx, .. } = &mut self.mode
                    && !self.possible_starting_sites.is_empty()
                {
                    *start_site_idx = Some(
                        (start_site_idx.unwrap_or_default() + self.possible_starting_sites.len()
                            - 1)
                            % self.possible_starting_sites.len(),
                    );
                }
            },
            Message::NextStartingSite => {
                if let Mode::CreateOrEdit { start_site_idx, .. } = &mut self.mode
                    && !self.possible_starting_sites.is_empty()
                {
                    *start_site_idx = Some(
                        (start_site_idx.unwrap_or_default()
                            + self.possible_starting_sites.len()
                            + 1)
                            % self.possible_starting_sites.len(),
                    );
                }
            },
            Message::WizardNext => {
                if let Mode::CreateOrEdit { step, .. } = &mut self.mode {
                    *step = step.next();
                }
            },
            Message::WizardBack => {
                if let Mode::CreateOrEdit { step, .. } = &mut self.mode {
                    *step = step.back();
                }
            },
        }
    }

    /// Get the character to display
    pub fn display_body_inventory<'a>(
        &'a self,
        characters: &'a [CharacterItem],
    ) -> Option<(comp::Body, &'a Inventory)> {
        match &self.mode {
            Mode::Select { .. } => self
                .selected
                .and_then(|id| characters.iter().find(|i| i.character.id == Some(id)))
                .map(|i| (i.body, &i.inventory)),
            Mode::CreateOrEdit {
                inventory, body, ..
            } => Some((comp::Body::Humanoid(*body), inventory)),
        }
    }
}

pub struct CharSelectionUi {
    ui: Ui,
    controls: Controls,
    enter_pressed: bool,
    select_character: Option<CharacterId>,
    pub error: Option<String>,
}

impl CharSelectionUi {
    pub fn new(global_state: &mut GlobalState, client: &Client) -> Self {
        // Load up the last selected character for this server
        let server_name = &client.server_info().name;
        let selected_character = global_state.profile.get_selected_character(server_name);

        // Load language
        let i18n = global_state.i18n.read();

        // TODO: don't add default font twice
        let font = ui::ice::load_font(&i18n.fonts().get("cyri").unwrap().asset_key);

        let mut ui = Ui::new(
            &mut global_state.window,
            font,
            global_state.settings.interface.ui_scale,
        )
        .unwrap();

        let fonts = Fonts::load(i18n.fonts(), &mut ui).expect("Impossible to load fonts");

        #[cfg(feature = "singleplayer")]
        let default_name = match global_state.singleplayer.is_running() {
            true => String::new(),
            false => global_state.settings.networking.username.clone(),
        };

        #[cfg(not(feature = "singleplayer"))]
        let default_name = global_state.settings.networking.username.clone();

        let controls = Controls::new(
            fonts,
            Imgs::load(&mut ui).expect("Failed to load images"),
            selected_character,
            default_name,
            client.server_info(),
            ui.add_graphic(Graphic::Image(
                Arc::clone(client.world_data().topo_map_image()),
                Some(default_water_color()),
            )),
            client
                .possible_starting_sites()
                .iter()
                .filter_map(|site_id| client.sites().get(site_id))
                .map(|info| info.marker.clone())
                .collect(),
            client.world_data().chunk_size().as_(),
            client.server_description().rules.is_some(),
        );

        Self {
            ui,
            controls,
            enter_pressed: false,
            select_character: None,
            error: None,
        }
    }

    pub fn display_body_inventory<'a>(
        &'a self,
        characters: &'a [CharacterItem],
    ) -> Option<(comp::Body, &'a Inventory)> {
        self.controls.display_body_inventory(characters)
    }

    pub fn handle_event(&mut self, event: window::Event) -> bool {
        match event {
            window::Event::IcedUi(event) => {
                // Enter Key pressed
                use iced::keyboard;
                if let iced::Event::Keyboard(keyboard::Event::KeyPressed {
                    key_code: keyboard::KeyCode::Enter,
                    ..
                }) = event
                {
                    self.enter_pressed = true;
                }

                self.ui.handle_event(event);
                true
            },
            window::Event::MouseButton(_, window::PressState::Pressed) => {
                !self.controls.mouse_detector.mouse_over()
            },
            window::Event::ScaleFactorChanged(s) => {
                self.ui.scale_factor_changed(s);
                false
            },
            _ => false,
        }
    }

    pub fn update_language(&mut self, i18n: LocalizationHandle) {
        let i18n = i18n.read();
        let font = ui::ice::load_font(&i18n.fonts().get("cyri").unwrap().asset_key);

        self.ui.clear_fonts(font);
        self.controls.fonts =
            Fonts::load(i18n.fonts(), &mut self.ui).expect("Impossible to load fonts!");
    }

    pub fn set_scale_mode(&mut self, scale_mode: ui::ScaleMode) {
        self.ui.set_scaling_mode(scale_mode);
    }

    pub fn select_character(&mut self, id: CharacterId) { self.select_character = Some(id); }

    pub fn display_error(&mut self, error: String) { self.error = Some(error); }

    // TODO: do we need whole client here or just character list?
    pub fn maintain(&mut self, global_state: &mut GlobalState, client: &Client) -> Vec<Event> {
        let mut events = Vec::new();
        let i18n = global_state.i18n.read();

        let (mut messages, _) = self.ui.maintain(
            self.controls
                .view(&global_state.settings, client, &self.error, &i18n),
            global_state.window.renderer_mut(),
            None,
            &mut global_state.clipboard,
        );

        if self.enter_pressed {
            self.enter_pressed = false;
            messages.push(match self.controls.mode {
                Mode::Select { .. } => Message::EnterWorld,
                Mode::CreateOrEdit { .. } => Message::CreateCharacter,
            });
        }

        if let Some(id) = self.select_character.take() {
            messages.push(Message::Select(id))
        }

        messages.into_iter().for_each(|message| {
            self.controls
                .update(message, &mut events, &client.character_list().characters)
        });

        events
    }

    pub fn render<'a>(&'a self, drawer: &mut UiDrawer<'_, 'a>) { self.ui.render(drawer); }
}

#[derive(Default)]
struct Sliders {
    hair_style: slider::State,
    hair_color: slider::State,
    skin: slider::State,
    eyes: slider::State,
    eye_color: slider::State,
    accessory: slider::State,
    beard: slider::State,
    starting_site: slider::State,
}

#[cfg(test)]
mod background_ui_tests {
    use super::*;

    // `long_name_threshold_catches_all_must_stay_names` (pre-curation
    // BL-31 UI-fixes spec §3) was removed during the 2026-07-02 catalogue
    // curation (see docs/design/specs/2026-07-02-backgrounds-curation-design.md
    // §4): it asserted that six "must-stay" long display names exceeded
    // `BACKGROUND_LONG_NAME_LEN`, but 5 of those 6 variants were cut and the
    // 6th (`UrbanBountyHunter`) was renamed to `BountyHunter` (13 chars,
    // under the threshold). No name in the surviving 24-background V1
    // catalogue exceeds the threshold, so the test's premise no longer holds
    // and it was deleted rather than rewritten against a now-empty guarantee.

    /// `background_stat_passive`/`background_starter_kit` must be total over
    /// `BackgroundKind::ALL` (spec §4.5) so the detail panel never shows a
    /// blank Habilidades/Items section — the `match` itself is exhaustive at
    /// compile time, so this test just guards against a future match arm
    /// returning an empty string by mistake.
    #[test]
    fn stat_passive_and_starter_kit_are_non_empty_for_all_backgrounds() {
        for kind in BackgroundKind::ALL {
            assert!(
                !background_stat_passive(kind).is_empty(),
                "{kind:?} has an empty stat passive description"
            );
            assert!(
                !background_starter_kit(kind).is_empty(),
                "{kind:?} has an empty starter kit description"
            );
        }
    }
}
