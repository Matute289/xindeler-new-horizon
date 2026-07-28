use iced::{Element, Hasher, Layout, Length, Point, Rectangle, Size, Widget, layout};
use std::hash::Hash;

const DEFAULT_FILL_FRACTION: f32 = 1.0;
const DEFAULT_VERTICAL_ADJUSTMENT: f32 = 0.05;
/// Font size never shrinks below this, no matter how long the label is —
/// past this point illegibility is worse than a small overflow.
const MIN_FONT_SIZE: u16 = 8;
/// How many halving-style steps `shrink_to_fit` takes at most. `measure`
/// isn't perfectly linear in size (kerning/hinting), so one shrink can
/// slightly over/undershoot; a few iterations converge without looping
/// forever.
const MAX_SHRINK_STEPS: u32 = 4;

/// Wraps the existing Text widget giving it more advanced layouting
/// capabilities
/// Centers child text widget and adjust the font size depending on the height
/// of the limits. Assumes single line text is being used — if the label is
/// too wide to fit on one line at the height-derived size, the font shrinks
/// (down to `MIN_FONT_SIZE`) rather than silently wrapping/clipping a second
/// line, since the caller's box height is fixed and won't grow to fit it.
pub struct FillText<R>
where
    R: iced::text::Renderer,
{
    //max_font_size: u16, uncomment if there is a use case for this
    /// Portion of the height of the limits which the font size should be
    fill_fraction: f32,
    /// Adjustment factor to center the text vertically
    /// Multiplied by font size and used to move the text up if positive
    // TODO: use the produced glyph geometry directly to do this and/or add support to
    // layouting library
    vertical_adjustment: f32,
    text: iced::Text<R>,
    /// Kept alongside `text` (whose `content`/`font` fields are private) so
    /// `shrink_to_fit` can call `Renderer::measure` directly.
    content: String,
    font: R::Font,
}

impl<R> FillText<R>
where
    R: iced::text::Renderer,
{
    pub fn new(label: impl Into<String>) -> Self {
        let content = label.into();
        Self {
            //max_font_size: u16::MAX,
            fill_fraction: DEFAULT_FILL_FRACTION,
            vertical_adjustment: DEFAULT_VERTICAL_ADJUSTMENT,
            text: iced::Text::new(content.clone()),
            content,
            font: R::Font::default(),
        }
    }

    /// Largest font size, at or below `max_font_size`, at which `content`
    /// measures no wider than `available_width` — or `MIN_FONT_SIZE` if it
    /// still doesn't fit there.
    fn shrink_to_fit(&self, renderer: &R, max_font_size: u16, available_width: f32) -> u16 {
        let mut font_size = max_font_size;
        for _ in 0..MAX_SHRINK_STEPS {
            if font_size <= MIN_FONT_SIZE {
                return MIN_FONT_SIZE;
            }
            let (measured_width, _) = renderer.measure(
                &self.content,
                font_size,
                self.font,
                Size::new(f32::INFINITY, f32::INFINITY),
            );
            if measured_width <= available_width || measured_width <= 0.0 {
                return font_size;
            }
            let scale = (available_width / measured_width).clamp(0.5, 0.95);
            font_size = ((font_size as f32 * scale) as u16).max(MIN_FONT_SIZE);
        }
        font_size
    }

    #[must_use]
    pub fn fill_fraction(mut self, fraction: f32) -> Self {
        self.fill_fraction = fraction;
        self
    }

    #[must_use]
    pub fn vertical_adjustment(mut self, adjustment: f32) -> Self {
        self.vertical_adjustment = adjustment;
        self
    }

    #[must_use]
    pub fn color(mut self, color: impl Into<iced::Color>) -> Self {
        self.text = self.text.color(color);
        self
    }

    #[must_use]
    pub fn font(mut self, font: impl Into<R::Font>) -> Self {
        let font = font.into();
        self.text = self.text.font(font);
        self.font = font;
        self
    }
}

impl<M, R> Widget<M, R> for FillText<R>
where
    R: iced::text::Renderer,
{
    fn width(&self) -> Length { Length::Fill }

    fn height(&self) -> Length { Length::Fill }

    fn layout(&self, renderer: &R, limits: &layout::Limits) -> layout::Node {
        let limits = limits.width(Length::Fill).height(Length::Fill);

        let size = limits.max();

        let font_size = self.shrink_to_fit(
            renderer,
            (size.height * self.fill_fraction) as u16,
            size.width,
        );

        let mut text =
            Widget::<M, _>::layout(&self.text.clone().size(font_size), renderer, &limits);

        // Size adjusted for centering
        text.align(
            iced::Align::Center,
            iced::Align::Center,
            Size::new(
                size.width,
                size.height - 2.0 * font_size as f32 * self.vertical_adjustment,
            ),
        );

        layout::Node::with_children(size, vec![text])
    }

    fn draw(
        &self,
        renderer: &mut R,
        defaults: &R::Defaults,
        layout: Layout<'_>,
        cursor_position: Point,
        viewport: &Rectangle,
    ) -> R::Output {
        // Note: this breaks if the parent widget adjusts the bounds height
        // TODO: this recomputes the identical shrink `layout` just did for the same
        // bounds. `glyph_brush`'s own cache absorbs the expensive part (perf-reviewed,
        // negligible at current FillText counts), but if `FillText` ever grows a
        // persistent `State`, cache the resulting font_size in a `Cell<u16>` in
        // `layout` and read it here instead of recomputing.
        let font_size = self.shrink_to_fit(
            renderer,
            (layout.bounds().height * self.fill_fraction) as u16,
            layout.bounds().width,
        );
        Widget::<M, _>::draw(
            &self.text.clone().size(font_size),
            renderer,
            defaults,
            layout.children().next().unwrap(),
            cursor_position,
            viewport,
        )
    }

    fn hash_layout(&self, state: &mut Hasher) {
        struct Marker;
        std::any::TypeId::of::<Marker>().hash(state);

        self.fill_fraction.to_bits().hash(state);
        self.vertical_adjustment.to_bits().hash(state);
        Widget::<M, R>::hash_layout(&self.text, state);
    }
}

impl<'a, M, R> From<FillText<R>> for Element<'a, M, R>
where
    R: 'a + iced::text::Renderer,
{
    fn from(fill_text: FillText<R>) -> Element<'a, M, R> { Element::new(fill_text) }
}
