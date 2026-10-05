//! `WinUI` 3 (Fluent) `Visuals` and `Style` for egui's built-in widgets.
//!
//! Buttons use the neutral control fill with a 1px control stroke, inputs a
//! recessed well with an accent focus stroke, and selection a tinted accent.
//! Corner radii, control height, scroll bars and shadows follow the `WinUI`
//! design tokens in [`super::metrics::FLUENT_METRICS`].

use egui::{Color32, CornerRadius, Margin, Shadow, Stroke, Style, Theme, Visuals, style};

use super::metrics::FLUENT_METRICS;
use super::palette::Palette;

/// Control fills (rest, hover, pressed) — `WinUI` `ControlFillColor*`.
const fn control_fills(theme: Theme) -> (Color32, Color32, Color32) {
    match theme {
        Theme::Dark => (
            Color32::from_rgb(0x2D, 0x2D, 0x2D),
            Color32::from_rgb(0x32, 0x32, 0x32),
            Color32::from_rgb(0x27, 0x27, 0x27),
        ),
        Theme::Light => (
            Color32::from_rgb(0xFB, 0xFB, 0xFB),
            Color32::from_rgb(0xF6, 0xF6, 0xF6),
            Color32::from_rgb(0xF5, 0xF5, 0xF5),
        ),
    }
}

/// Builds the Fluent [`Style`] for `theme`, starting from `base` so text
/// styles registered by the font setup are preserved.
#[must_use]
pub fn style(base: &Style, theme: Theme, palette: &Palette) -> Style {
    let mut style = base.clone();
    style.visuals = visuals(theme, palette);

    let m = FLUENT_METRICS;
    style.spacing.item_spacing = egui::vec2(8.0, 8.0);
    style.spacing.button_padding = egui::vec2(11.0, 5.0);
    style.spacing.interact_size.y = m.control_height;
    style.spacing.indent = 16.0;
    style.spacing.window_margin = Margin::same(16);
    style.spacing.menu_margin = Margin::same(4);
    style.spacing.scroll = style::ScrollStyle::floating();
    style
}

fn visuals(theme: Theme, p: &Palette) -> Visuals {
    let m = FLUENT_METRICS;
    let (rest, hover, pressed) = control_fills(theme);
    let control = CornerRadius::same(m.control_radius);

    let mut v = match theme {
        Theme::Dark => Visuals::dark(),
        Theme::Light => Visuals::light(),
    };
    v.override_text_color = None;
    v.weak_text_color = Some(p.hint_text);
    v.hyperlink_color = p.accent;
    v.panel_fill = p.panel_bg;
    v.window_fill = p.window_bg;
    v.faint_bg_color = p.card_bg;
    v.extreme_bg_color = p.extreme_bg;
    v.text_edit_bg_color = Some(p.extreme_bg);
    v.code_bg_color = p.card_bg;
    v.warn_fg_color = p.caution;
    v.error_fg_color = p.critical;
    v.selection.bg_fill = p.selection_bg;
    v.selection.stroke = Stroke::new(1.0, p.accent);
    v.slider_trailing_fill = true;
    v.handle_shape = style::HandleShape::Circle;
    v.window_stroke = Stroke::new(1.0, p.border);
    v.window_corner_radius = CornerRadius::same(m.overlay_radius);
    v.menu_corner_radius = CornerRadius::same(m.overlay_radius);
    v.striped = false;

    let shadow_alpha = if theme == Theme::Dark { 0x60 } else { 0x24 };
    v.window_shadow = Shadow {
        offset: [0, 16],
        blur: 48,
        spread: 0,
        color: Color32::from_black_alpha(shadow_alpha),
    };
    v.popup_shadow = Shadow {
        offset: [0, 8],
        blur: 16,
        spread: 0,
        color: Color32::from_black_alpha(shadow_alpha),
    };

    let w = &mut v.widgets;
    w.noninteractive.bg_fill = p.card_bg;
    w.noninteractive.weak_bg_fill = p.card_bg;
    w.noninteractive.bg_stroke = Stroke::new(1.0, p.border_subtle);
    w.noninteractive.fg_stroke = Stroke::new(1.0, p.text);
    w.noninteractive.corner_radius = control;

    w.inactive.bg_fill = p.control_track;
    w.inactive.weak_bg_fill = rest;
    w.inactive.bg_stroke = Stroke::new(1.0, p.border);
    w.inactive.fg_stroke = Stroke::new(1.0, p.text);
    w.inactive.corner_radius = control;
    w.inactive.expansion = 0.0;

    w.hovered.bg_fill = p.control_track;
    w.hovered.weak_bg_fill = hover;
    w.hovered.bg_stroke = Stroke::new(1.0, p.border);
    w.hovered.fg_stroke = Stroke::new(1.0, p.text);
    w.hovered.corner_radius = control;
    w.hovered.expansion = 0.0;

    w.active.bg_fill = p.accent;
    w.active.weak_bg_fill = pressed;
    w.active.bg_stroke = Stroke::new(1.0, p.border);
    w.active.fg_stroke = Stroke::new(1.0, p.text_weak);
    w.active.corner_radius = control;
    w.active.expansion = 0.0;

    w.open.bg_fill = pressed;
    w.open.weak_bg_fill = pressed;
    w.open.bg_stroke = Stroke::new(1.0, p.border);
    w.open.fg_stroke = Stroke::new(1.0, p.text);
    w.open.corner_radius = control;
    w.open.expansion = 0.0;
    v
}
