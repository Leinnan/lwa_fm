//! The few colors and shapes that set the window's areas apart: the window
//! itself is the backdrop, and what holds content sits on it as a surface
//! with an edge.
use windows_reactor::{Brush, Color, TextBlock, ThemeBrush, ToggleSwitch};

/// The corner radius of a surface.
pub const RADIUS: f64 = 8.0;
/// The space between surfaces, and between them and the window's edge.
pub const GAP: f64 = 6.0;

/// The fill of an area that holds content: a folder, the inspector.
pub fn surface() -> Brush {
    ThemeBrush::CardBackground.into()
}

/// The edge of a surface, and the lines that divide it.
pub fn stroke() -> Brush {
    ThemeBrush::CardStroke.into()
}

/// The fill of a box to read a path from or type in, a shade off its surface.
pub fn field() -> Brush {
    Color::argb(0x1A, 0x80, 0x80, 0x80).into()
}

/// A tag's colour, bright enough on a light window and a dark one.
pub const fn tag_color(color: dirfleet_core::app::TagColor) -> Color {
    use dirfleet_core::app::TagColor;
    match color {
        TagColor::Red => Color::rgb(0xE5, 0x48, 0x4D),
        TagColor::Orange => Color::rgb(0xF2, 0x8C, 0x28),
        TagColor::Yellow => Color::rgb(0xE5, 0xB8, 0x00),
        TagColor::Green => Color::rgb(0x3F, 0xA9, 0x57),
        TagColor::Blue => Color::rgb(0x3B, 0x82, 0xF6),
        TagColor::Purple => Color::rgb(0x9B, 0x5D, 0xE5),
        TagColor::Gray => Color::rgb(0x8A, 0x8F, 0x98),
    }
}

/// The edge of a surface that has the focus among several.
pub fn focused_stroke() -> Brush {
    ThemeBrush::Accent.into()
}

/// A switch without the system's own words beside it ("On", "Off" in the
/// system's language): its label already says what it turns on.
pub fn switch(on: bool) -> ToggleSwitch {
    ToggleSwitch::new()
        // The control keeps room for those words unless told otherwise.
        .min_width(0.0)
        .is_on(on)
        .on_content(TextBlock::new())
        .off_content(TextBlock::new())
}
