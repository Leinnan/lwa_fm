//! The settings the core acts on, kept between launches, as egui's: the
//! terminal, theme, icon size, asking before the Trash, what the sidebar
//! shows, whether selected previews play, and the view new folders get.
use crux_core::Command;
use crux_core::render::render;
use facet::Facet;
use serde::{Deserialize, Serialize};

use super::{DirFleet, Effect, Event, FolderView, Model};

#[derive(Facet, Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq, Default)]
#[repr(C)]
pub enum Appearance {
    #[default]
    System,
    Light,
    Dark,
}

#[derive(Facet, Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq, Default)]
#[repr(C)]
pub enum IconSize {
    Small,
    #[default]
    Medium,
    Large,
    ExtraLarge,
}

impl IconSize {
    pub const ALL: [Self; 4] = [Self::Small, Self::Medium, Self::Large, Self::ExtraLarge];

    pub const fn label(self) -> &'static str {
        match self {
            Self::Small => "Small",
            Self::Medium => "Medium",
            Self::Large => "Large",
            Self::ExtraLarge => "Extra large",
        }
    }

    /// The grid's icon edge, in points.
    pub const fn points(self) -> u32 {
        match self {
            Self::Small => 40,
            Self::Medium => 56,
            Self::Large => 80,
            Self::ExtraLarge => 112,
        }
    }
}

#[derive(Facet, Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
#[serde(default)]
#[allow(clippy::struct_excessive_bools)]
pub struct Settings {
    /// The terminal app Open in Terminal starts: a name on macOS
    /// ("Terminal", "iTerm"), a program elsewhere.
    pub terminal: String,
    pub appearance: Appearance,
    pub icon_size: IconSize,
    /// Ask before moving items to the Trash.
    pub confirm_trash: bool,
    /// Ask whether to move or copy items dropped on a folder.
    pub confirm_drop: bool,
    pub show_recent: bool,
    pub show_trash: bool,
    /// Show how full each drive is under its sidebar row.
    pub show_drive_capacity: bool,
    /// Say when a drive is connected or disconnected.
    pub notify_drive_changes: bool,
    /// Count the files opened from the app, for "Date opened" and "Opens".
    pub track_open_history: bool,
    /// Play the selected grid tile's video or GIF, not only the hovered one.
    pub animate_selected_previews: bool,
    /// Suggest Full Disk Access at launch while it is off (macOS).
    pub ask_full_disk_access: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            terminal: if cfg!(target_os = "macos") {
                "Terminal".into()
            } else if cfg!(windows) {
                "cmd".into()
            } else {
                "x-terminal-emulator".into()
            },
            appearance: Appearance::System,
            icon_size: IconSize::Medium,
            confirm_trash: false,
            confirm_drop: false,
            show_recent: true,
            show_trash: true,
            show_drive_capacity: true,
            notify_drive_changes: true,
            track_open_history: true,
            animate_selected_previews: false,
            ask_full_disk_access: true,
        }
    }
}

/// An icon size and its label, for menus.
#[derive(Facet, Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct IconSizeChoice {
    pub size: IconSize,
    pub label: String,
}

pub(super) fn icon_size_choices() -> Vec<IconSizeChoice> {
    IconSize::ALL
        .into_iter()
        .map(|size| IconSizeChoice {
            size,
            label: size.label().to_owned(),
        })
        .collect()
}

impl DirFleet {
    /// The events that change settings.
    pub(super) fn settings_event(model: &mut Model, event: Event) -> Command<Effect, Event> {
        match event {
            Event::ChangeSettings { settings } => {
                let terminal_changed = settings.terminal != model.settings.terminal;
                model.settings = settings;
                if terminal_changed {
                    return render().and(Self::check_terminal(model));
                }
            }
            Event::RestoreDefaultSettings => {
                model.settings = Settings::default();
                model.default_view = FolderView::default();
            }
            Event::SetFolderDefaults { view } => model.default_view = view,
            _ => return Command::done(),
        }
        render()
    }
}
