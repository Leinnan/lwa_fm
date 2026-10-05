//! Windows Fluent integration. See PROVENANCE.md for the source revision.
mod fluent;
mod fonts;
pub mod metrics;
pub mod palette;
#[cfg(windows)]
pub mod win32;

use egui::{Context, Id};
use palette::AccentShades;

#[derive(Clone, Copy)]
pub struct Config {
    pub fluent: bool,
    pub system_fonts: bool,
    pub accent: AccentShades,
}

impl Config {
    pub fn platform() -> Self {
        static ACCENT: std::sync::OnceLock<AccentShades> = std::sync::OnceLock::new();
        Self {
            fluent: cfg!(windows),
            system_fonts: true,
            accent: *ACCENT.get_or_init(AccentShades::system),
        }
    }
}

pub fn config(ctx: &Context) -> Config {
    ctx.data(|data| data.get_temp::<Config>(Id::new("theme_config")))
        .unwrap_or_else(Config::platform)
}

pub fn configure(ctx: &Context, config: Config) {
    ctx.data_mut(|data| data.insert_temp(Id::new("theme_config"), config));
    if config.fluent {
        ctx.set_fonts(fonts::definitions(config.system_fonts));
    }
    super::ui::configure(ctx);
}

pub fn palette(ctx: &Context, dark: bool) -> palette::Palette {
    let accent = config(ctx).accent;
    if dark {
        palette::fluent_dark(accent)
    } else {
        palette::fluent_light(accent)
    }
}

pub fn apply_styles(ctx: &Context) {
    let accent = config(ctx).accent;
    let dark_palette = palette::fluent_dark(accent);
    let light_palette = palette::fluent_light(accent);
    ctx.all_styles_mut(|style| {
        let dark = style.visuals.dark_mode;
        fonts::type_scale(style);
        *style = fluent::style(
            style,
            if dark {
                egui::Theme::Dark
            } else {
                egui::Theme::Light
            },
            if dark { &dark_palette } else { &light_palette },
        );
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn system_theme_changes_live_but_saved_overrides_remain_authoritative() {
        use super::super::ui::Appearance;
        let ctx = Context::default();
        super::super::ui::configure_test(&ctx, true);
        for (appearance, system_theme, expected) in [
            (Appearance::System, egui::Theme::Dark, egui::Theme::Dark),
            (Appearance::System, egui::Theme::Light, egui::Theme::Light),
            (Appearance::Dark, egui::Theme::Light, egui::Theme::Dark),
            (Appearance::Light, egui::Theme::Dark, egui::Theme::Light),
        ] {
            let saved = serde_json::to_string(&appearance).expect("serialize legacy appearance");
            let restored: Appearance = serde_json::from_str(&saved).expect("restore appearance");
            restored.apply(&ctx);
            let mut output = ctx.run_ui(
                egui::RawInput {
                    system_theme: Some(system_theme),
                    ..Default::default()
                },
                |_| {},
            );
            output.textures_delta.clear();
            assert_eq!(ctx.theme(), expected);
            assert_eq!(
                ctx.global_style().visuals.panel_fill,
                palette(&ctx, expected == egui::Theme::Dark).panel_bg
            );
        }
    }

    #[test]
    fn both_styles_match_the_injected_accent_and_facade() {
        let ctx = Context::default();
        let accent = AccentShades {
            light2: egui::Color32::from_rgb(180, 240, 160),
            dark1: egui::Color32::from_rgb(30, 90, 20),
            ..AccentShades::WINDOWS_DEFAULT
        };
        configure(
            &ctx,
            Config {
                fluent: true,
                system_fonts: false,
                accent,
            },
        );
        for (theme, expected) in [
            (egui::Theme::Dark, accent.light2),
            (egui::Theme::Light, accent.dark1),
        ] {
            let style = ctx.style_of(theme);
            let facade = super::super::ui::Palette::for_context(&ctx, theme == egui::Theme::Dark);
            assert_eq!(style.visuals.hyperlink_color, expected);
            assert_eq!(style.visuals.panel_fill, facade.content);
            assert_eq!(style.visuals.text_color(), facade.text);
            assert_eq!(style.visuals.weak_text_color(), facade.secondary);
            assert_eq!(style.visuals.selection.stroke.color, facade.accent);
            assert!((style.spacing.interact_size.y - 32.0).abs() < f32::EPSILON);
            assert!((style.text_styles[&egui::TextStyle::Body].size - 14.0).abs() < f32::EPSILON);
            assert_eq!(
                style.visuals.window_corner_radius,
                egui::CornerRadius::same(8)
            );
        }
        super::super::ui::configure_test(&ctx, false);
        assert!(!config(&ctx).fluent);
        assert!(
            (ctx.style_of(egui::Theme::Dark).text_styles[&egui::TextStyle::Body].size - 13.0).abs()
                < f32::EPSILON
        );
    }
}
