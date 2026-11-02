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
    pub const ALL: [Self; 3] = [Self::System, Self::Light, Self::Dark];

    pub const fn label(self) -> &'static str {
        match self {
            Self::System => "Use system setting",
            Self::Light => "Light",
            Self::Dark => "Dark",
        }
    }

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
    pub card_border: Color32,
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
                card_border: Color32::from_rgb(61, 61, 65),
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
                card_border: Color32::from_rgb(216, 216, 222),
                border: Color32::from_rgb(216, 216, 222),
                text: Color32::from_rgb(30, 30, 34),
                secondary: Color32::from_rgb(100, 100, 108),
                accent: Color32::from_rgb(0, 112, 232),
            }
        }
    }
    pub fn of(ui: &Ui) -> Self {
        Self::for_context(ui.ctx(), ui.visuals().dark_mode)
    }
    #[cfg_attr(not(any(windows, test)), allow(clippy::missing_const_for_fn))]
    pub fn for_context(ctx: &Context, dark: bool) -> Self {
        #[cfg(any(windows, test))]
        if super::theme::config(ctx).fluent {
            let p = super::theme::palette(ctx, dark);
            return Self {
                content: p.panel_bg,
                sidebar: p.sidebar_bg,
                toolbar: p.toolbar_bg,
                card: p.card_bg,
                card_border: p.card_stroke,
                border: p.border,
                text: p.text,
                secondary: p.text_weak,
                accent: p.accent,
            };
        }
        #[cfg(not(any(windows, test)))]
        let _ = ctx;
        Self::for_theme(dark)
    }
}

pub fn configure(ctx: &Context) {
    #[cfg(any(windows, test))]
    if super::theme::config(ctx).fluent {
        super::theme::apply_styles(ctx);
        return;
    }

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
        // The same as `control_height`, so buttons line up with text fields.
        style.spacing.interact_size.y = 28.0;
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
        egui::WidgetInfo::selected(egui::WidgetType::Button, ui.is_enabled(), active, name)
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

/// How a dialog button is drawn.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ButtonKind {
    /// The dialog's main action, filled with the accent color.
    Primary,
    /// A main action that destroys data, filled red.
    Danger,
    Plain,
}

/// One button of [`dialog_buttons`].
#[derive(Clone, Copy, Debug)]
pub struct DialogButton<'a> {
    pub label: &'a str,
    pub kind: ButtonKind,
    pub enabled: bool,
}

impl<'a> DialogButton<'a> {
    pub const fn primary(label: &'a str) -> Self {
        Self {
            label,
            kind: ButtonKind::Primary,
            enabled: true,
        }
    }
    pub const fn danger(label: &'a str) -> Self {
        Self {
            label,
            kind: ButtonKind::Danger,
            enabled: true,
        }
    }
    pub const fn plain(label: &'a str) -> Self {
        Self {
            label,
            kind: ButtonKind::Plain,
            enabled: true,
        }
    }
    pub const fn enabled(mut self, enabled: bool) -> Self {
        self.enabled = enabled;
        self
    }
}

/// The button row at the bottom of a dialog: right-aligned, as on Windows,
/// with `buttons` in reading order (main action first, Cancel last) and of
/// equal minimum width. With `focus_first`, the first enabled button takes
/// keyboard focus so Enter runs it; dialogs with a text field leave it off.
/// Returns the index of the clicked button.
pub fn dialog_buttons(ui: &mut Ui, focus_first: bool, buttons: &[DialogButton]) -> Option<usize> {
    ui.add_space(12.0);
    let height = control_height(ui);
    let mut clicked = None;
    ui.allocate_ui_with_layout(
        Vec2::new(ui.available_width(), height),
        egui::Layout::right_to_left(egui::Align::Center),
        |ui| {
            ui.spacing_mut().item_spacing.x = 8.0;
            let first = buttons.iter().position(|button| button.enabled);
            // Right to left, so the last button is added first.
            for (index, button) in buttons.iter().enumerate().rev() {
                let fill = match button.kind {
                    _ if !button.enabled => None,
                    ButtonKind::Primary => Some(Palette::of(ui).accent),
                    ButtonKind::Danger => Some(Color32::from_rgb(196, 43, 28)),
                    ButtonKind::Plain => None,
                };
                let widget = fill.map_or_else(
                    || egui::Button::new(button.label),
                    |fill| {
                        // Fluent's dark accent is light; keep the label readable on it.
                        let text = if fill.intensity() > 0.6 {
                            Color32::BLACK
                        } else {
                            Color32::WHITE
                        };
                        egui::Button::new(RichText::new(button.label).color(text))
                            .fill(fill)
                            .stroke(Stroke::NONE)
                    },
                );
                let response =
                    ui.add_enabled(button.enabled, widget.min_size(Vec2::new(88.0, height)));
                if focus_first && Some(index) == first && ui.memory(|m| m.focused().is_none()) {
                    response.request_focus();
                }
                if response.has_focus() {
                    ui.painter().rect_stroke(
                        response.rect.expand(2.0),
                        control_radius(ui),
                        Stroke::new(1.0_f32, ui.visuals().strong_text_color()),
                        egui::StrokeKind::Outside,
                    );
                }
                if response.clicked()
                    || (response.has_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)))
                {
                    clicked = Some(index);
                }
            }
        },
    );
    clicked
}

pub fn card(ui: &Ui) -> Frame {
    Frame::new()
        .fill(Palette::of(ui).card)
        .stroke(Stroke::new(1.0_f32, Palette::of(ui).card_border))
        .corner_radius(card_radius(ui))
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
/// A [`section`] heading that can be clicked to collapse the rows below it
/// and dragged to reorder; the chevron shows whether it is collapsed.
pub fn section_header(ui: &mut Ui, title: &str, collapsed: bool) -> Response {
    ui.add_space(8.0);
    let (rect, response) = ui.allocate_exact_size(
        Vec2::new(ui.available_width(), 22.0),
        egui::Sense::click_and_drag(),
    );
    response.widget_info(|| {
        egui::WidgetInfo::selected(egui::WidgetType::Button, true, !collapsed, title)
    });
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
    let color = Palette::of(ui).secondary;
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
        .fill(Palette::of(ui).card)
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
        .fill(Palette::of(ui).card)
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
                        egui::WidgetInfo::selected(
                            egui::WidgetType::RadioButton,
                            enabled,
                            on,
                            label,
                        )
                    });
                    if response.clicked() && !on {
                        next = Some(index);
                    }
                }
            });
        });
    next
}

/// An on/off switch, as Windows and macOS settings use for options that
/// apply at once. `label` names it for screen readers and tests.
pub fn toggle_switch(ui: &mut Ui, on: &mut bool, label: &str) -> Response {
    let (rect, mut response) = ui.allocate_exact_size(Vec2::new(40.0, 20.0), egui::Sense::click());
    if response.clicked() {
        *on = !*on;
        response.mark_changed();
    }
    let checked = *on;
    response.widget_info(|| {
        egui::WidgetInfo::selected(egui::WidgetType::Checkbox, ui.is_enabled(), checked, label)
    });
    if ui.is_rect_visible(rect) {
        let t = ui.ctx().animate_bool_responsive(response.id, checked);
        let palette = Palette::of(ui);
        let dim = |color: Color32| {
            if ui.is_enabled() {
                color
            } else {
                color.gamma_multiply(0.45)
            }
        };
        let hovered = response.hovered() && ui.is_enabled();
        let (fill, stroke, knob) = if checked {
            let accent = if hovered {
                palette.accent.lerp_to_gamma(palette.text, 0.12)
            } else {
                palette.accent
            };
            (dim(accent), Stroke::NONE, dim(Color32::WHITE))
        } else {
            let fill = if hovered {
                palette.text.gamma_multiply(0.08)
            } else {
                Color32::TRANSPARENT
            };
            (
                fill,
                Stroke::new(1.0_f32, dim(palette.secondary)),
                dim(palette.secondary),
            )
        };
        let painter = ui.painter();
        let radius = rect.height() / 2.0;
        painter.rect(rect, radius, fill, stroke, egui::StrokeKind::Inside);
        let x = egui::lerp((rect.left() + radius)..=(rect.right() - radius), t);
        let knob_radius = if hovered { 6.0 } else { 5.0 };
        painter.circle_filled(egui::pos2(x, rect.center().y), knob_radius, knob);
        if response.has_focus() {
            painter.rect_stroke(
                rect.expand(2.0),
                radius + 2.0,
                Stroke::new(1.5_f32, palette.accent),
                egui::StrokeKind::Outside,
            );
        }
    }
    response
}

/// One group of settings: a heading and a full-width card whose rows are
/// separated by thin lines. Add rows with [`SettingsGroup::row`].
pub fn settings_group<R>(
    ui: &mut Ui,
    title: &str,
    rows: impl FnOnce(&mut SettingsGroup<'_>) -> R,
) -> R {
    ui.add_space(14.0);
    ui.label(
        RichText::new(title)
            .strong()
            .color(Palette::of(ui).secondary),
    );
    ui.add_space(6.0);
    let palette = Palette::of(ui);
    Frame::new()
        .fill(palette.card)
        .stroke(Stroke::new(1.0_f32, palette.card_border))
        .corner_radius(card_radius(ui))
        .inner_margin(egui::Margin::symmetric(16, 0))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.spacing_mut().item_spacing.y = 0.0;
            rows(&mut SettingsGroup { ui, rows: 0 })
        })
        .inner
}

/// The rows of a [`settings_group`].
pub struct SettingsGroup<'a> {
    pub ui: &'a mut Ui,
    rows: usize,
}

impl SettingsGroup<'_> {
    /// A setting: its title and an optional description on the left, its
    /// control (built by `control`, laid out right to left) on the right.
    pub fn row<R>(
        &mut self,
        title: &str,
        description: Option<&str>,
        control: impl FnOnce(&mut Ui) -> R,
    ) -> R {
        if self.rows > 0 {
            let x = self.ui.max_rect().x_range();
            let y = self.ui.cursor().top();
            self.ui.painter().hline(
                x.min - 16.0..=x.max + 16.0,
                y,
                Stroke::new(1.0_f32, Palette::of(self.ui).card_border),
            );
        }
        self.rows += 1;
        setting_row(self.ui, title, description, control)
    }

    /// A row of explanatory text with no control.
    pub fn note(&mut self, text: &str) {
        self.row("", Some(text), |_| {});
    }
}

/// The height of a settings row with no description.
fn setting_row_height(ui: &Ui) -> f32 {
    if is_fluent(ui.ctx()) { 48.0 } else { 44.0 }
}

fn setting_row<R>(
    ui: &mut Ui,
    title: &str,
    description: Option<&str>,
    control: impl FnOnce(&mut Ui) -> R,
) -> R {
    let palette = Palette::of(ui);
    let width = ui.available_width();
    let top_left = ui.cursor().min;
    let min_height = setting_row_height(ui);
    // The control goes first, right-aligned, so the text can wrap in the
    // space it leaves.
    let control_area = egui::Rect::from_min_size(
        egui::pos2(width.mul_add(0.4, top_left.x), top_left.y),
        Vec2::new(width * 0.6, min_height),
    );
    let mut control_ui = ui.new_child(
        egui::UiBuilder::new()
            .max_rect(control_area)
            .layout(egui::Layout::right_to_left(egui::Align::Center)),
    );
    let inner = control(&mut control_ui);
    let used = control_ui.min_rect();
    let control_width = if used.width() > 0.0 {
        used.width() + 16.0
    } else {
        0.0
    };
    let text_width = (width - control_width).max(80.0);

    let title_galley = ui.painter().layout(
        title.to_owned(),
        TextStyle::Body.resolve(ui.style()),
        palette.text,
        text_width,
    );
    let description_galley = description.map(|text| {
        ui.painter().layout(
            text.to_owned(),
            TextStyle::Small.resolve(ui.style()),
            palette.secondary,
            text_width,
        )
    });
    // A note has no title, only its (description) text.
    let title_height = if title.is_empty() {
        0.0
    } else {
        title_galley.size().y + 2.0
    };
    let text_height = title_height
        + description_galley
            .as_ref()
            .map_or(0.0, |galley| galley.size().y);
    let height = min_height.max(text_height + 16.0);
    let rect = egui::Rect::from_min_size(top_left, Vec2::new(width, height));
    ui.allocate_rect(rect, egui::Sense::hover());
    let mut y = rect.center().y - text_height / 2.0;
    if !title.is_empty() {
        ui.painter()
            .galley(egui::pos2(rect.left(), y), title_galley, palette.text);
    }
    y += title_height;
    if let Some(galley) = description_galley {
        ui.painter()
            .galley(egui::pos2(rect.left(), y), galley, palette.secondary);
    }
    inner
}

/// Key combinations drawn as key caps, e.g. "Ctrl+F", "F3". In a right
/// to left row, pass them last first.
pub fn key_caps(ui: &mut Ui, combinations: &[String]) {
    let palette = Palette::of(ui);
    let font = FontId::monospace(12.0);
    for keys in combinations {
        let galley = ui
            .painter()
            .layout_no_wrap(keys.clone(), font.clone(), palette.text);
        let size = galley.size() + Vec2::new(12.0, 6.0);
        let (rect, response) = ui.allocate_exact_size(size, egui::Sense::hover());
        response.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Label, true, keys));
        let painter = ui.painter();
        painter.rect(
            rect,
            4,
            palette.card,
            Stroke::new(1.0_f32, palette.card_border),
            egui::StrokeKind::Inside,
        );
        // A slightly heavier bottom edge, like a key.
        painter.hline(
            rect.x_range().shrink(2.0),
            rect.bottom() - 0.5,
            Stroke::new(1.0_f32, palette.secondary.gamma_multiply(0.5)),
        );
        painter.galley(rect.min + Vec2::new(6.0, 3.0), galley, palette.text);
    }
}

#[cfg(test)]
pub fn snapshot_options() -> egui_kittest::SnapshotOptions {
    let options = egui_kittest::SnapshotOptions::new();
    if cfg!(windows) {
        options.output_path("tests/snapshots/windows")
    } else {
        options
    }
}

#[cfg_attr(not(any(windows, test)), allow(clippy::missing_const_for_fn))]
pub fn is_fluent(ctx: &Context) -> bool {
    #[cfg(any(windows, test))]
    {
        super::theme::config(ctx).fluent
    }
    #[cfg(not(any(windows, test)))]
    {
        let _ = ctx;
        false
    }
}
pub fn control_height(ui: &Ui) -> f32 {
    if is_fluent(ui.ctx()) { 32.0 } else { 28.0 }
}
/// The top bar's padding; tighter around Fluent's taller controls.
pub fn toolbar_margin(ui: &Ui) -> egui::Margin {
    egui::Margin::symmetric(12, if is_fluent(ui.ctx()) { 6 } else { 8 })
}
/// A one-line field as tall as the other controls, with its text centered.
pub fn text_field<'t>(ui: &Ui, text: &'t mut dyn egui::TextBuffer) -> egui::TextEdit<'t> {
    egui::TextEdit::singleline(text)
        .min_size(Vec2::new(0.0, control_height(ui)))
        .vertical_align(egui::Align::Center)
        .margin(text_field_margin(ui))
}
pub fn text_field_margin(ui: &Ui) -> egui::Margin {
    if is_fluent(ui.ctx()) {
        egui::Margin::symmetric(4, 2)
    } else {
        egui::Margin::symmetric(8, 2)
    }
}
#[cfg_attr(not(any(windows, test)), allow(clippy::missing_const_for_fn))]
pub fn control_radius(ui: &Ui) -> u8 {
    #[cfg(any(windows, test))]
    if is_fluent(ui.ctx()) {
        return super::theme::metrics::FLUENT_METRICS.control_radius;
    }
    let _ = ui;
    6
}
#[cfg_attr(not(any(windows, test)), allow(clippy::missing_const_for_fn))]
pub fn card_radius(ui: &Ui) -> u8 {
    #[cfg(any(windows, test))]
    if is_fluent(ui.ctx()) {
        return super::theme::metrics::FLUENT_METRICS.card_radius;
    }
    let _ = ui;
    10
}
/// A sidebar row: icon and one line of text, with hover, selection and
/// focus states. Used by every sidebar section so they look the same.
pub fn nav_row(
    ui: &mut Ui,
    icon: Icon,
    label: &str,
    selected: bool,
    sense: egui::Sense,
) -> Response {
    let (rect, response) =
        ui.allocate_exact_size(Vec2::new(ui.available_width(), nav_row_height(ui)), sense);
    response.widget_info(|| {
        egui::WidgetInfo::selected(egui::WidgetType::SelectableLabel, true, selected, label)
    });
    if selected || response.hovered() || response.has_focus() {
        ui.painter()
            .rect_filled(rect, control_radius(ui), navigation_fill(ui, selected));
    }
    if selected && is_fluent(ui.ctx()) {
        ui.painter().rect_filled(
            egui::Rect::from_center_size(
                egui::pos2(rect.left() + 3.0, rect.center().y),
                Vec2::new(3.0, 16.0),
            ),
            2,
            Palette::of(ui).accent,
        );
    }
    if response.has_focus() {
        ui.painter().rect_stroke(
            rect,
            control_radius(ui),
            Stroke::new(1.0_f32, Palette::of(ui).accent),
            egui::StrokeKind::Inside,
        );
    }
    ui.painter().text(
        rect.left_center() + Vec2::new(16.0, 0.0),
        egui::Align2::CENTER_CENTER,
        char::from(icon),
        FontId::new(14.0, egui::FontFamily::Name("lucide".into())),
        Palette::of(ui).secondary,
    );
    let mut job = egui::text::LayoutJob::simple_singleline(
        label.to_owned(),
        TextStyle::Body.resolve(ui.style()),
        Palette::of(ui).text,
    );
    job.wrap.max_width = (rect.width() - 42.0).max(0.0);
    job.wrap.max_rows = 1;
    job.wrap.break_anywhere = true;
    let galley = ui.fonts_mut(|fonts| fonts.layout_job(job));
    ui.painter().galley(
        rect.left_center() + Vec2::new(32.0, -galley.size().y / 2.0),
        galley,
        Palette::of(ui).text,
    );
    response
}

#[cfg_attr(not(any(windows, test)), allow(clippy::missing_const_for_fn))]
pub fn nav_row_height(ui: &Ui) -> f32 {
    #[cfg(any(windows, test))]
    if is_fluent(ui.ctx()) {
        return super::theme::metrics::FLUENT_METRICS.nav_row_height;
    }
    let _ = ui;
    28.0
}
pub fn navigation_fill(ui: &Ui, selected: bool) -> Color32 {
    #[cfg(any(windows, test))]
    if is_fluent(ui.ctx()) {
        let p = super::theme::palette(ui.ctx(), ui.visuals().dark_mode);
        return if selected {
            p.subtle_selected
        } else {
            p.subtle_hover
        };
    }
    if selected {
        ui.visuals().selection.bg_fill
    } else {
        Palette::of(ui).card
    }
}
#[cfg(test)]
pub fn configure_test(ctx: &Context, fluent: bool) {
    super::theme::configure(
        ctx,
        super::theme::Config {
            fluent,
            system_fonts: false,
            accent: super::theme::palette::AccentShades::WINDOWS_DEFAULT,
        },
    );
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
