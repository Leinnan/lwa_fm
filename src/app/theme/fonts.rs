//! Segoe and Cascadia font loading adapted from the source Fluent theme.
use egui::{FontData, FontDefinitions, FontFamily, FontId, Style, TextStyle};

pub fn definitions(system_fonts: bool) -> FontDefinitions {
    let dir = std::env::var_os("WINDIR")
        .map_or_else(|| "C:\\Windows".into(), std::path::PathBuf::from)
        .join("Fonts");
    definitions_with_loader(|names| {
        if system_fonts && cfg!(windows) {
            names
                .iter()
                .find_map(|name| std::fs::read(dir.join(name)).ok())
        } else {
            None
        }
    })
}

fn definitions_with_loader(mut load: impl FnMut(&[&str]) -> Option<Vec<u8>>) -> FontDefinitions {
    let mut fonts = FontDefinitions::default();
    super::super::ui::register_icons(&mut fonts);
    for (name, family, candidates) in [
        (
            "segoe",
            FontFamily::Proportional,
            &["SegUIVar.ttf", "segoeui.ttf"][..],
        ),
        (
            "cascadia",
            FontFamily::Monospace,
            &["CascadiaMono.ttf", "consola.ttf"][..],
        ),
    ] {
        if let Some(bytes) = load(candidates) {
            fonts
                .font_data
                .insert(name.into(), FontData::from_owned(bytes).into());
            fonts
                .families
                .entry(family)
                .or_default()
                .insert(0, name.into());
        }
    }
    let mut semibold = fonts.families[&FontFamily::Proportional].clone();
    if let Some(bytes) = load(&["seguisb.ttf"]) {
        fonts
            .font_data
            .insert("semibold".into(), FontData::from_owned(bytes).into());
        semibold.insert(0, "semibold".into());
    }
    fonts
        .families
        .insert(FontFamily::Name("semibold".into()), semibold);
    fonts
}

pub fn type_scale(style: &mut Style) {
    style.text_styles = [
        (TextStyle::Small, FontId::proportional(12.0)),
        (TextStyle::Body, FontId::proportional(14.0)),
        (TextStyle::Button, FontId::proportional(14.0)),
        (
            TextStyle::Heading,
            FontId::new(20.0, FontFamily::Name("semibold".into())),
        ),
        (TextStyle::Monospace, FontId::monospace(13.0)),
    ]
    .into();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_system_fonts_keep_defaults_and_separate_icons() {
        let fonts = definitions_with_loader(|_| None);
        for family in [
            FontFamily::Proportional,
            FontFamily::Monospace,
            FontFamily::Name("semibold".into()),
        ] {
            assert!(!fonts.families[&family].is_empty());
            assert!(!fonts.families[&family].iter().any(|name| name == "lucide"));
        }
        assert_eq!(
            fonts.families[&FontFamily::Name("lucide".into())],
            ["lucide"]
        );
    }

    #[cfg(windows)]
    #[test]
    fn installed_system_fonts_render_with_the_fallback_chain() {
        let ctx = egui::Context::default();
        super::super::configure(&ctx, super::super::Config::platform());
        for _ in 0..2 {
            let mut output = ctx.run_ui(egui::RawInput::default(), |ui| {
                ui.heading("Segoe UI · Zażółć gęślą jaźń");
                ui.label("Fluent body and captions");
                ui.monospace("Cascadia Mono / Consolas");
                ui.label(super::super::super::ui::glyph(lucide_icons::Icon::Folder));
            });
            output.textures_delta.clear();
        }
    }
}
