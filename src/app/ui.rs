//! Shared appearance and controls for the file browser.
use egui::{Color32, Context, FontId, Frame, Response, RichText, Stroke, TextStyle, Ui, Vec2};
pub use egui_mev_theme::icons::glyph;
use egui_mev_theme::{AccentShades, FontOptions, IconFont, Metrics, Theme, ThemeFamily};
pub use egui_mev_theme::{Appearance, Palette};
use egui_mev_widgets::card_frame;
pub use egui_mev_widgets::{
    DialogButton, dialog_buttons, key_caps, nav_row, settings_group, text_field, toggle_switch,
};
use lucide_icons::Icon;

/// The toolkit's sizes, kept dense for a file manager.
const GRAPHITE_METRICS: Metrics = Metrics {
    card_radius: 10,
    overlay_radius: 10,
    control_height: 28.0,
    interact_height: 28.0,
    nav_row_height: 28.0,
    caption: 11.0,
    card_padding: 12,
    row_height: 28.0,
    ..Metrics::GRAPHITE
};
const FLUENT_METRICS: Metrics = Metrics {
    card_radius: 8,
    title: 20.0,
    row_height: 32.0,
    ..Metrics::FLUENT
};

fn app_theme(family: ThemeFamily, accent: AccentShades) -> Theme {
    Theme::for_family(family, accent).with_metrics(match family {
        ThemeFamily::Graphite => GRAPHITE_METRICS,
        ThemeFamily::Fluent => FLUENT_METRICS,
    })
}

fn icon_fonts() -> FontOptions {
    FontOptions {
        icons: vec![IconFont::Lucide],
        ..Default::default()
    }
}

/// Tighter rows than the toolkit's Graphite, and text that stays readable.
fn adjust_style(ctx: &Context) {
    let theme = Theme::of(ctx);
    if theme.family != ThemeFamily::Graphite {
        return;
    }
    ctx.all_styles_mut(|style| {
        style.spacing.item_spacing = Vec2::new(8.0, 4.0);
        style.spacing.button_padding = Vec2::new(8.0, 4.0);
        let palette = theme.palette(style.visuals.dark_mode);
        // Without it, selected controls draw accent text on an accent fill.
        style.visuals.override_text_color = Some(palette.text);
        style.visuals.weak_text_color = Some(palette.text_weak);
        // Row stripes, faint enough for long file lists.
        style.visuals.faint_bg_color = if style.visuals.dark_mode {
            Color32::from_white_alpha(5)
        } else {
            Color32::from_black_alpha(3)
        };
    });
}

/// The look of the OS this runs on, around the user's accent colour.
pub fn install_theme(ctx: &Context) {
    app_theme(ThemeFamily::platform_default(), AccentShades::system())
        .install_with_fonts(ctx, &icon_fonts());
    adjust_style(ctx);
}

pub fn icon_button(ui: &mut Ui, icon: Icon, label: &str, active: bool) -> Response {
    icon_button_named(ui, icon, label, label, active)
}
/// An icon button with an accessible `name` and a separate `tooltip`, so a
/// shortcut can be shown without being read out as part of the name. It
/// keeps one size whether or not it is active.
pub fn icon_button_named(
    ui: &mut Ui,
    icon: Icon,
    name: &str,
    tooltip: &str,
    active: bool,
) -> Response {
    let side = control_height(ui);
    let response = ui.add_sized(
        [side, side],
        egui::Button::new(glyph(icon))
            .selected(active)
            .frame(active),
    );
    response.widget_info(|| {
        egui::WidgetInfo::selected(egui::Role::Button, ui.is_enabled(), active, name)
    });
    if response.has_focus() {
        ui.painter().rect_stroke(
            response.rect,
            control_radius(ui),
            Stroke::new(1.0_f32, Palette::of(ui).accent),
            egui::StrokeKind::Inside,
        );
    }
    response.on_hover_text(tooltip)
}
/// An icon button that opens a menu, drawn like the other icon buttons.
pub fn icon_menu_button(
    ui: &mut Ui,
    icon: Icon,
    name: &str,
    add_contents: impl FnOnce(&mut Ui),
) -> Response {
    let response = icon_button_named(ui, icon, name, name, false);
    egui::Popup::menu(&response).show(add_contents);
    response
}
/// The frame of a dialog: a popup with room around its contents.
pub fn dialog_frame(ctx: &Context) -> Frame {
    Frame::popup(&ctx.global_style()).inner_margin(20)
}

pub fn card(ui: &Ui) -> Frame {
    card_frame(ui)
}
pub fn section(ui: &mut Ui, title: &str) {
    ui.add_space(12.0);
    ui.label(
        RichText::new(title.to_uppercase())
            .small()
            .strong()
            .color(Palette::of(ui).text_weak),
    );
    ui.add_space(4.0);
}
/// A [`section`] heading that can be clicked to collapse the rows below it
/// and dragged to reorder; the chevron shows whether it is collapsed.
pub fn section_header(ui: &mut Ui, title: &str, collapsed: bool) -> Response {
    ui.add_space(8.0);
    let (rect, response) = ui.allocate_exact_size(
        Vec2::new(ui.available_width(), 22.0),
        egui::Sense::click_and_drag(),
    );
    response
        .widget_info(|| egui::WidgetInfo::selected(egui::Role::Button, true, !collapsed, title));
    if response.hovered() || response.has_focus() {
        ui.painter()
            .rect_filled(rect, control_radius(ui), navigation_fill(ui, false));
    }
    if response.has_focus() {
        ui.painter().rect_stroke(
            rect,
            control_radius(ui),
            Stroke::new(1.0_f32, Palette::of(ui).accent),
            egui::StrokeKind::Inside,
        );
    }
    let color = Palette::of(ui).text_weak;
    ui.painter().text(
        rect.left_center() + Vec2::new(4.0, 0.0),
        egui::Align2::LEFT_CENTER,
        title.to_uppercase(),
        TextStyle::Small.resolve(ui.style()),
        color,
    );
    let chevron = if collapsed {
        Icon::ChevronRight
    } else {
        Icon::ChevronDown
    };
    ui.painter().text(
        rect.right_center() - Vec2::new(10.0, 0.0),
        egui::Align2::CENTER_CENTER,
        char::from(chevron),
        FontId::new(12.0, egui::FontFamily::Name("lucide".into())),
        color,
    );
    ui.add_space(2.0);
    response
}
pub fn badge(ui: &mut Ui, text: &str) {
    Frame::new()
        .fill(Palette::of(ui).card_bg)
        .corner_radius(8)
        .inner_margin(egui::Margin::symmetric(8, 2))
        .show(ui, |ui| {
            ui.label(RichText::new(text).small());
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
/// A segmented choice (a radio group) of text labels: one of `choices`
/// (label, enabled, tooltip) is on. Returns the newly clicked one.
pub fn segmented_text(
    ui: &mut Ui,
    choices: &[(&str, bool, &str)],
    selected: usize,
) -> Option<usize> {
    let mut next = None;
    // In a right-aligned row the buttons are laid out right to left, so add
    // them last first to keep them reading left to right.
    let mut order: Vec<usize> = (0..choices.len()).collect();
    if ui.layout().prefer_right_to_left() {
        order.reverse();
    }
    Frame::new()
        .fill(Palette::of(ui).card_bg)
        .corner_radius(control_radius(ui))
        .inner_margin(2)
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 2.0;
                for index in order {
                    let (label, enabled, tip) = choices[index];
                    let on = index == selected;
                    let mut response = ui.add_enabled(enabled, egui::Button::selectable(on, label));
                    if !tip.is_empty() {
                        response = response.on_hover_text(tip).on_disabled_hover_text(tip);
                    }
                    response.widget_info(|| {
                        egui::WidgetInfo::selected(egui::Role::RadioButton, enabled, on, label)
                    });
                    if response.clicked() && !on {
                        next = Some(index);
                    }
                }
            });
        });
    next
}

pub fn is_fluent(ctx: &Context) -> bool {
    ThemeFamily::of_ctx(ctx) == ThemeFamily::Fluent
}
pub fn control_height(ui: &Ui) -> f32 {
    Metrics::of(ui).control_height
}
/// The top bar's padding; tighter around Fluent's taller controls.
pub fn toolbar_margin(ui: &Ui) -> egui::Margin {
    egui::Margin::symmetric(12, Metrics::of(ui).toolbar_margin)
}
pub fn control_radius(ui: &Ui) -> u8 {
    Metrics::of(ui).control_radius
}
pub fn card_radius(ui: &Ui) -> u8 {
    Metrics::of(ui).card_radius
}
pub fn navigation_fill(ui: &Ui, selected: bool) -> Color32 {
    if is_fluent(ui.ctx()) {
        let p = Palette::of(ui);
        return if selected {
            p.subtle_selected
        } else {
            p.subtle_hover
        };
    }
    if selected {
        ui.visuals().selection.bg_fill
    } else {
        Palette::of(ui).card_bg
    }
}
#[cfg(test)]
pub fn configure_test(ctx: &Context, fluent: bool) {
    // Some harnesses call this every frame, and rebuilding the fonts would dominate
    // their frame times.
    let installed = egui::Id::unique("test_theme_installed");
    if ctx.data(|data| data.get_temp::<bool>(installed)) == Some(fluent) {
        return;
    }
    ctx.data_mut(|data| data.insert_temp(installed, fluent));
    let (family, accent) = if fluent {
        (ThemeFamily::Fluent, AccentShades::WINDOWS_DEFAULT)
    } else {
        (ThemeFamily::Graphite, AccentShades::MACOS_DEFAULT)
    };
    egui_mev_test::install_with(ctx, app_theme(family, accent), &icon_fonts());
    adjust_style(ctx);
    if fluent {
        ctx.all_styles_mut(|style| {
            style.text_styles.insert(
                TextStyle::Heading,
                FontId::new(
                    FLUENT_METRICS.title,
                    egui::FontFamily::Name(egui_mev_theme::fonts::SEMIBOLD.into()),
                ),
            );
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    /// Settings saved by older versions name these variants, so they must not change.
    #[test]
    fn saved_appearance_names_still_load() {
        for (name, appearance) in [
            ("\"System\"", Appearance::System),
            ("\"Light\"", Appearance::Light),
            ("\"Dark\"", Appearance::Dark),
        ] {
            assert_eq!(
                serde_json::from_str::<Appearance>(name).expect("known name"),
                appearance
            );
            assert_eq!(
                serde_json::to_string(&appearance).expect("serializes"),
                name
            );
        }
    }
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
