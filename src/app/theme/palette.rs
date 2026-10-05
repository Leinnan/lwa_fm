//! Fluent palette and accent shades adapted from `wihaister_full`; see PROVENANCE.md.
use egui::Color32;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Palette {
    pub accent: Color32,
    pub border: Color32,
    pub border_subtle: Color32,
    pub card_bg: Color32,
    pub card_bg_hover: Color32,
    pub card_stroke: Color32,
    pub caution: Color32,
    pub control_track: Color32,
    pub critical: Color32,
    pub extreme_bg: Color32,
    pub hint_text: Color32,
    pub panel_bg: Color32,
    pub selection_bg: Color32,
    pub sidebar_bg: Color32,
    pub subtle_hover: Color32,
    pub subtle_selected: Color32,
    pub text: Color32,
    pub text_weak: Color32,
    pub toolbar_bg: Color32,
    pub window_bg: Color32,
}

/// The seven shades of a Windows accent colour, lightest first, as stored in
/// the `AccentPalette` registry value.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AccentShades {
    /// `SystemAccentColorLight3`.
    pub light3: Color32,
    /// `SystemAccentColorLight2` — the accent fill in dark mode.
    pub light2: Color32,
    /// `SystemAccentColorLight1`.
    pub light1: Color32,
    /// `SystemAccentColor`.
    pub base: Color32,
    /// `SystemAccentColorDark1` — the accent fill in light mode.
    pub dark1: Color32,
    /// `SystemAccentColorDark2`.
    pub dark2: Color32,
    /// `SystemAccentColorDark3`.
    pub dark3: Color32,
}

impl AccentShades {
    /// The Windows 11 default blue accent.
    pub const WINDOWS_DEFAULT: Self = Self {
        light3: Color32::from_rgb(0x99, 0xEB, 0xFF),
        light2: Color32::from_rgb(0x60, 0xCD, 0xFF),
        light1: Color32::from_rgb(0x00, 0x91, 0xF8),
        base: Color32::from_rgb(0x00, 0x78, 0xD4),
        dark1: Color32::from_rgb(0x00, 0x5F, 0xB8),
        dark2: Color32::from_rgb(0x00, 0x3E, 0x92),
        dark3: Color32::from_rgb(0x00, 0x1A, 0x68),
    };

    /// Parses the 32-byte `AccentPalette` registry value: eight RGBA
    /// quadruplets, lightest first (the eighth entry is unused).
    ///
    /// Returns `None` when the value is shorter than seven colours.
    #[must_use]
    pub fn from_accent_palette_bytes(bytes: &[u8]) -> Option<Self> {
        let color = |index: usize| {
            let chunk = bytes.get(index * 4..index * 4 + 3)?;
            Some(Color32::from_rgb(chunk[0], chunk[1], chunk[2]))
        };
        Some(Self {
            light3: color(0)?,
            light2: color(1)?,
            light1: color(2)?,
            base: color(3)?,
            dark1: color(4)?,
            dark2: color(5)?,
            dark3: color(6)?,
        })
    }

    /// The system accent shades: read from the registry on Windows, the
    /// Windows default blue elsewhere or when the value is unavailable.
    #[must_use]
    pub fn system() -> Self {
        #[cfg(windows)]
        if let Some(shades) = super::win32::system_accent() {
            return shades;
        }
        Self::WINDOWS_DEFAULT
    }
}

/// Builds the Fluent (`WinUI` 3) dark palette around `accent`.
///
/// Translucent `WinUI` brushes are pre-blended over the Mica base `#202020`.
#[must_use]
pub fn fluent_dark(accent: AccentShades) -> Palette {
    let fill = accent.light2;
    Palette {
        text: Color32::from_rgb(0xFF, 0xFF, 0xFF),
        text_weak: Color32::from_rgb(0xCF, 0xCF, 0xCF),

        hint_text: Color32::from_rgb(0x9D, 0x9D, 0x9D),
        panel_bg: Color32::from_rgb(0x27, 0x27, 0x27),
        sidebar_bg: Color32::from_rgb(0x20, 0x20, 0x20),
        toolbar_bg: Color32::from_rgb(0x20, 0x20, 0x20),
        window_bg: Color32::from_rgb(0x2C, 0x2C, 0x2C),
        card_bg: Color32::from_rgb(0x2E, 0x2E, 0x2E),
        card_bg_hover: Color32::from_rgb(0x34, 0x34, 0x34),
        card_stroke: Color32::from_rgb(0x1C, 0x1C, 0x1C),

        extreme_bg: Color32::from_rgb(0x1F, 0x1F, 0x1F),
        control_track: Color32::from_rgb(0x5C, 0x5C, 0x5C),
        border: Color32::from_rgb(0x3C, 0x3C, 0x3C),
        border_subtle: Color32::from_rgb(0x35, 0x35, 0x35),
        accent: fill,

        selection_bg: accent
            .base
            .lerp_to_gamma(Color32::from_rgb(0x27, 0x27, 0x27), 0.45),
        subtle_hover: Color32::from_rgb(0x2D, 0x2D, 0x2D),
        subtle_selected: Color32::from_rgb(0x32, 0x32, 0x32),

        caution: Color32::from_rgb(0xFC, 0xE1, 0x00),
        critical: Color32::from_rgb(0xFF, 0x99, 0xA4),
    }
}

/// Builds the Fluent (`WinUI` 3) light palette around `accent`.
///
/// Translucent `WinUI` brushes are pre-blended over the Mica base `#F3F3F3`.
#[must_use]
pub fn fluent_light(accent: AccentShades) -> Palette {
    let fill = accent.dark1;
    Palette {
        text: Color32::from_rgb(0x1B, 0x1B, 0x1B),
        text_weak: Color32::from_rgb(0x61, 0x61, 0x61),

        hint_text: Color32::from_rgb(0x8A, 0x8A, 0x8A),
        panel_bg: Color32::from_rgb(0xF9, 0xF9, 0xF9),
        sidebar_bg: Color32::from_rgb(0xF3, 0xF3, 0xF3),
        toolbar_bg: Color32::from_rgb(0xF3, 0xF3, 0xF3),
        window_bg: Color32::from_rgb(0xF9, 0xF9, 0xF9),
        card_bg: Color32::from_rgb(0xFF, 0xFF, 0xFF),
        card_bg_hover: Color32::from_rgb(0xF6, 0xF6, 0xF6),
        card_stroke: Color32::from_rgb(0xE5, 0xE5, 0xE5),

        extreme_bg: Color32::from_rgb(0xFF, 0xFF, 0xFF),
        control_track: Color32::from_rgb(0xC4, 0xC4, 0xC4),
        border: Color32::from_rgb(0xE0, 0xE0, 0xE0),
        border_subtle: Color32::from_rgb(0xE5, 0xE5, 0xE5),
        accent: fill,

        selection_bg: accent.base.lerp_to_gamma(Color32::WHITE, 0.75),
        subtle_hover: Color32::from_rgb(0xEA, 0xEA, 0xEA),
        subtle_selected: Color32::from_rgb(0xE5, 0xE5, 0xE5),

        caution: Color32::from_rgb(0x9D, 0x5D, 0x00),
        critical: Color32::from_rgb(0xC4, 0x2B, 0x1C),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accent_palette_reads_rgb_and_ignores_alpha_and_extra_color() {
        let mut bytes = [0_u8; 32];
        for (i, color) in bytes.as_chunks_mut::<4>().0.iter_mut().enumerate() {
            color.copy_from_slice(&[i as u8, 100, 200, 0]);
        }
        let accent =
            AccentShades::from_accent_palette_bytes(&bytes).expect("complete accent palette");
        assert_eq!(accent.base, Color32::from_rgb(3, 100, 200));
        assert_eq!(accent.dark3, Color32::from_rgb(6, 100, 200));
        assert_eq!(fluent_dark(accent).accent, accent.light2);
        assert_eq!(fluent_light(accent).accent, accent.dark1);
        for length in 0..27 {
            assert_eq!(
                AccentShades::from_accent_palette_bytes(&bytes[..length]),
                None
            );
        }
        assert_eq!(
            AccentShades::from_accent_palette_bytes(&bytes[..27]),
            Some(accent)
        );
        assert_eq!(
            fluent_light(AccentShades::WINDOWS_DEFAULT).accent,
            Color32::from_rgb(0, 95, 184)
        );
    }
}
