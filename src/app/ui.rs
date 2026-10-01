//! Shared appearance and controls for the file browser.
use egui::{Color32, Context, FontId, Frame, Response, RichText, Stroke, TextStyle, Ui, Vec2};
use lucide_icons::Icon;
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum Appearance {
    #[default]
    System,
    Light,
    Dark,
}

impl Appearance {
    pub fn apply(self, ctx: &Context) {
        ctx.set_theme(match self {
            Self::System => egui::ThemePreference::System,
            Self::Light => egui::ThemePreference::Light,
            Self::Dark => egui::ThemePreference::Dark,
        });
    }
}

#[derive(Clone, Copy)]
pub struct Palette {
    pub content: Color32,
    pub sidebar: Color32,
    pub toolbar: Color32,
    pub card: Color32,
    pub border: Color32,
    pub text: Color32,
    pub secondary: Color32,
    pub accent: Color32,
}
impl Palette {
    pub const fn for_theme(dark: bool) -> Self {
        if dark {
            Self {
                content: Color32::from_rgb(30, 30, 32),
                sidebar: Color32::from_rgb(38, 38, 40),
                toolbar: Color32::from_rgb(43, 43, 45),
                card: Color32::from_rgb(48, 48, 51),
                border: Color32::from_rgb(61, 61, 65),
                text: Color32::from_rgb(236, 236, 240),
                secondary: Color32::from_rgb(152, 152, 158),
                accent: Color32::from_rgb(10, 132, 255),
            }
        } else {
            Self {
                content: Color32::from_rgb(255, 255, 255),
                sidebar: Color32::from_rgb(242, 242, 245),
                toolbar: Color32::from_rgb(247, 247, 249),
                card: Color32::from_rgb(250, 250, 252),
                border: Color32::from_rgb(216, 216, 222),
                text: Color32::from_rgb(30, 30, 34),
                secondary: Color32::from_rgb(100, 100, 108),
                accent: Color32::from_rgb(0, 112, 232),
            }
        }
    }
    pub fn of(ui: &Ui) -> Self {
        Self::for_theme(ui.visuals().dark_mode)
    }
}

pub fn configure(ctx: &Context) {
    ctx.all_styles_mut(|style| {
        let p = Palette::for_theme(style.visuals.dark_mode);
        style
            .text_styles
            .insert(TextStyle::Body, FontId::proportional(13.0));
        style
            .text_styles
            .insert(TextStyle::Button, FontId::proportional(13.0));
        style
            .text_styles
            .insert(TextStyle::Small, FontId::proportional(11.0));
        style
            .text_styles
            .insert(TextStyle::Heading, FontId::proportional(17.0));
        style
            .text_styles
            .insert(TextStyle::Monospace, FontId::monospace(13.0));
        style.spacing.item_spacing = Vec2::new(8.0, 4.0);
        style.spacing.button_padding = Vec2::new(8.0, 4.0);
        style.spacing.interact_size.y = 26.0;
        style.visuals.panel_fill = p.content;
        style.visuals.window_fill = p.sidebar;
        style.visuals.window_stroke = Stroke::new(1.0_f32, p.border);
        style.visuals.window_corner_radius = egui::CornerRadius::same(10);
        style.visuals.override_text_color = Some(p.text);
        style.visuals.weak_text_color = Some(p.secondary);
        style.visuals.faint_bg_color = if style.visuals.dark_mode {
            Color32::from_white_alpha(5)
        } else {
            Color32::from_black_alpha(3)
        };
        style.visuals.selection.bg_fill = if style.visuals.dark_mode {
            Color32::from_rgb(29, 78, 137)
        } else {
            Color32::from_rgb(210, 230, 255)
        };
        style.visuals.selection.stroke = Stroke::new(1.0_f32, p.accent);
        style.visuals.hyperlink_color = p.accent;
        for widget in [
            &mut style.visuals.widgets.inactive,
            &mut style.visuals.widgets.hovered,
            &mut style.visuals.widgets.active,
        ] {
            widget.corner_radius = egui::CornerRadius::same(6);
            widget.fg_stroke = Stroke::new(1.0_f32, p.text);
        }
        style.visuals.widgets.inactive.bg_fill = p.card;
        style.visuals.widgets.inactive.weak_bg_fill = p.card;
        style.visuals.widgets.inactive.bg_stroke = Stroke::new(1.0_f32, p.border);
        style.visuals.widgets.hovered.bg_fill = p.border;
        style.visuals.widgets.hovered.weak_bg_fill = p.border;
        style.visuals.widgets.active.bg_stroke = Stroke::new(1.0_f32, p.accent);
    });
}

pub fn register_icons(fonts: &mut egui::FontDefinitions) {
    fonts.font_data.insert(
        "lucide".into(),
        egui::FontData::from_static(lucide_icons::LUCIDE_FONT_BYTES).into(),
    );
    fonts.families.insert(
        egui::FontFamily::Name("lucide".into()),
        vec!["lucide".into()],
    );
}
pub fn glyph(icon: Icon) -> RichText {
    RichText::new(char::from(icon).to_string())
        .family(egui::FontFamily::Name("lucide".into()))
        .size(16.0)
}
pub fn icon_button(ui: &mut Ui, icon: Icon, label: &str, active: bool) -> Response {
    let response = ui.add(
        egui::Button::new(glyph(icon))
            .selected(active)
            .frame(active)
            .min_size(Vec2::new(28.0, 28.0)),
    );
    response.widget_info(|| {
        egui::WidgetInfo::selected(egui::WidgetType::Button, ui.is_enabled(), active, label)
    });
    if response.has_focus() {
        ui.painter().rect_stroke(
            response.rect,
            6,
            Stroke::new(1.0_f32, Palette::of(ui).accent),
            egui::StrokeKind::Inside,
        );
    }
    response.on_hover_text(label)
}
pub fn card(ui: &Ui) -> Frame {
    Frame::new()
        .fill(Palette::of(ui).card)
        .stroke(Stroke::new(1.0_f32, Palette::of(ui).border))
        .corner_radius(10)
        .inner_margin(12)
}
pub fn section(ui: &mut Ui, title: &str) {
    ui.add_space(12.0);
    ui.label(
        RichText::new(title.to_uppercase())
            .small()
            .strong()
            .color(Palette::of(ui).secondary),
    );
    ui.add_space(4.0);
}
pub fn badge(ui: &mut Ui, text: &str) {
    Frame::new()
        .fill(Palette::of(ui).card)
        .corner_radius(8)
        .inner_margin(egui::Margin::symmetric(8, 2))
        .show(ui, |ui| {
            ui.label(RichText::new(text).small());
        });
}
pub fn form_row(ui: &mut Ui, label: &str, content: impl FnOnce(&mut Ui)) {
    ui.horizontal(|ui| {
        ui.add_sized([110.0, 26.0], egui::Label::new(label));
        content(ui);
    });
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PanelLayout {
    pub sidebar: bool,
    pub inspector: bool,
    pub compact: bool,
}
impl PanelLayout {
    pub fn new(width: f32, sidebar: bool, inspector: bool) -> Self {
        Self {
            sidebar: width >= 800.0 && sidebar,
            inspector: width >= 1000.0 && inspector,
            compact: width < 800.0,
        }
    }
}
pub fn segmented_icons(ui: &mut Ui, choices: &[(Icon, &str)], selected: usize) -> Option<usize> {
    let mut next = None;
    Frame::new()
        .fill(Palette::of(ui).card)
        .corner_radius(6)
        .inner_margin(2)
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 0.0;
                for (index, (icon, label)) in choices.iter().enumerate() {
                    if icon_button(ui, *icon, label, index == selected).clicked() {
                        next = Some(index);
                    }
                }
            });
        });
    next
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn panels_collapse_without_changing_preferences() {
        assert_eq!(
            PanelLayout::new(1100.0, true, true),
            PanelLayout {
                sidebar: true,
                inspector: true,
                compact: false
            }
        );
        assert!(!PanelLayout::new(999.0, true, true).inspector);
        assert!(!PanelLayout::new(799.0, true, true).sidebar);
        assert!(PanelLayout::new(640.0, true, true).compact);
    }
}
