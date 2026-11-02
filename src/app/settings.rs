use std::{path::Path, process::Command};

use egui::{Modal, Response, RichText, Ui, Vec2, vec2};
use lucide_icons::Icon;
use serde::{Deserialize, Serialize};

use crate::{
    app::{
        DisplayType, Sort,
        assets::IconSize,
        directory_view_settings::{DirectoryShowHidden, DirectoryViewSettings},
    },
    fileops::trash_bin,
    helper::DataHolder,
};

use super::{
    commands::ActionToPerform,
    ui::{self, Appearance, Palette},
};

/// The pages of the settings dialog.
#[derive(Clone, Copy, Default, PartialEq, Eq)]
enum SettingsCategory {
    #[default]
    General,
    Appearance,
    Files,
    Previews,
    Keyboard,
    Plugins,
}

impl SettingsCategory {
    const ALL: [Self; 6] = [
        Self::General,
        Self::Appearance,
        Self::Files,
        Self::Previews,
        Self::Keyboard,
        Self::Plugins,
    ];

    const fn label(self) -> &'static str {
        match self {
            Self::General => "General",
            Self::Appearance => "Appearance",
            Self::Files => "Files & folders",
            Self::Previews => "Previews",
            Self::Keyboard => "Keyboard",
            Self::Plugins => "Plugins",
        }
    }

    const fn icon(self) -> Icon {
        match self {
            Self::General => Icon::Settings,
            Self::Appearance => Icon::Palette,
            Self::Files => Icon::Folder,
            Self::Previews => Icon::Image,
            Self::Keyboard => Icon::Keyboard,
            Self::Plugins => Icon::Puzzle,
        }
    }
}

/// The groups of rows in the sidebar, which can be collapsed and reordered.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum SidebarSection {
    Favorites,
    SavedSearches,
    Recent,
    Locations,
    Drives,
}

impl SidebarSection {
    pub const ALL: [Self; 5] = [
        Self::Favorites,
        Self::SavedSearches,
        Self::Recent,
        Self::Locations,
        Self::Drives,
    ];

    pub const fn label(self) -> &'static str {
        match self {
            Self::Favorites => "Favorites",
            Self::SavedSearches => "Saved searches",
            Self::Recent => "Recent",
            Self::Locations => "Locations",
            Self::Drives => "Drives",
        }
    }
}

/// Width of the category list on the left of the dialog.
const NAV_WIDTH: f32 = 184.0;
/// Below this dialog width the category list becomes a row of tabs.
const NARROW_WIDTH: f32 = 640.0;
/// How long "Restore defaults" waits for the confirming second click.
const RESTORE_CONFIRM_SECONDS: f64 = 3.0;
/// Width of drop-downs in setting rows, so their edges line up.
const CHOICE_WIDTH: f32 = 160.0;

#[derive(Serialize, Deserialize)]
#[serde(default)]
#[allow(clippy::struct_excessive_bools)]
pub struct ApplicationSettings {
    pub terminal_path: String,
    pub icon_size: IconSize,
    pub animate_selected_previews: bool,
    pub appearance: Appearance,
    pub sidebar_visible: bool,
    pub inspector_visible: bool,
    pub sidebar_width: f32,
    pub inspector_width: f32,
    /// Ask before moving items to the Recycle Bin.
    pub confirm_trash: bool,
    /// Ask before moving or copying items dropped onto a folder.
    pub confirm_drop: bool,
    /// Remember which files are opened from the app, for "Date opened" and
    /// "Times opened".
    pub track_open_history: bool,
    /// Show the recently opened folders in the sidebar.
    pub show_recent_folders: bool,
    /// Show the Recycle Bin / Trash in the sidebar.
    pub show_trash: bool,
    /// Show how full each drive is under its sidebar row.
    pub show_drive_capacity: bool,
    /// Say when a removable drive is connected or disconnected.
    pub notify_drive_changes: bool,
    /// The order of the sidebar sections, top to bottom.
    pub sidebar_order: Vec<SidebarSection>,
    /// The sidebar sections showing only their header.
    pub collapsed_sections: Vec<SidebarSection>,
    /// Suggest Full Disk Access at launch while the app lacks it (macOS).
    pub ask_full_disk_access: bool,
    /// Each plugin's enabled state, approval, shortcuts and form values.
    pub plugins: crate::plugins::host::SettingsMap,
    /// The page shown when the dialog opens: the last one used.
    #[serde(skip)]
    category: SettingsCategory,
    /// "Restore defaults" was clicked once on this page at this time and
    /// waits for the confirming click.
    #[serde(skip)]
    restore_armed: Option<(SettingsCategory, f64)>,
    /// "Clear open history" was clicked once at this time and waits for the
    /// confirming click.
    #[serde(skip)]
    clear_history_armed: Option<f64>,
}

impl Default for ApplicationSettings {
    fn default() -> Self {
        Self {
            #[cfg(not(target_os = "macos"))]
            terminal_path: "C:\\Program Files\\Alacritty\\alacritty.exe".into(),
            #[cfg(target_os = "macos")]
            terminal_path: "Terminal".into(),
            icon_size: IconSize::default(),
            animate_selected_previews: false,
            appearance: Appearance::System,
            sidebar_visible: true,
            inspector_visible: false,
            sidebar_width: 200.0,
            inspector_width: 280.0,
            confirm_trash: false,
            confirm_drop: true,
            track_open_history: true,
            show_recent_folders: true,
            show_trash: true,
            show_drive_capacity: true,
            notify_drive_changes: true,
            sidebar_order: SidebarSection::ALL.to_vec(),
            collapsed_sections: Vec::new(),
            ask_full_disk_access: true,
            plugins: crate::plugins::host::SettingsMap::new(),
            category: SettingsCategory::General,
            restore_armed: None,
            clear_history_armed: None,
        }
    }
}

/// The dialog's size: fixed while it is open, so switching pages never
/// moves or resizes it.
fn dialog_size(ctx: &egui::Context) -> Vec2 {
    let screen = ctx.content_rect().size();
    vec2(
        (screen.x - 48.0).clamp(560.0, 760.0).min(screen.x - 16.0),
        (screen.y - 96.0).clamp(360.0, 560.0).min(screen.y - 16.0),
    )
}

/// A drop-down of `all` values shown by `label`. Returns whether it changed.
fn choice<T: Copy + PartialEq>(
    ui: &mut Ui,
    id: &str,
    value: &mut T,
    all: &[T],
    label: fn(T) -> &'static str,
) -> bool {
    let before = *value;
    egui::ComboBox::from_id_salt(id)
        .width(CHOICE_WIDTH)
        .selected_text(label(*value))
        .show_ui(ui, |ui| {
            for &option in all {
                ui.selectable_value(value, option, label(option));
            }
        });
    *value != before
}

/// Saves the default view for new folders when it changed, and tells open
/// tabs to pick it up.
fn apply_directory_defaults(
    ui: &Ui,
    view: DirectoryViewSettings,
    hidden: DirectoryShowHidden,
    before: (Sort, bool, bool, DisplayType),
) {
    if before != (view.sorting, view.invert_sort, hidden.0, view.display_type) {
        ui.data_set_persisted(view);
        ui.data_set_persisted(hidden);
        ActionToPerform::ViewSettingsChanged(crate::app::DataSource::Settings).schedule();
    }
}

impl ApplicationSettings {
    #[allow(clippy::unused_self)]
    pub fn open_in_terminal<P>(&self, directory: P) -> std::io::Result<std::process::Child>
    where
        P: AsRef<Path>,
    {
        #[cfg(target_os = "macos")]
        {
            Command::new("open")
                .current_dir(directory)
                .arg("-a")
                .arg(&self.terminal_path)
                .arg(".")
                .spawn()
        }
        #[cfg(not(target_os = "macos"))]
        {
            Command::new(&self.terminal_path)
                .current_dir(directory)
                .spawn()
        }
    }

    /// The sidebar sections top to bottom: the saved order without
    /// duplicates, followed by any section it is missing.
    pub fn sidebar_order(&self) -> Vec<SidebarSection> {
        let mut order: Vec<SidebarSection> = Vec::with_capacity(SidebarSection::ALL.len());
        for section in self.sidebar_order.iter().chain(&SidebarSection::ALL) {
            if !order.contains(section) {
                order.push(*section);
            }
        }
        order
    }

    /// Moves `section` to `to`, an index in [`Self::sidebar_order`].
    pub fn move_sidebar_section(&mut self, section: SidebarSection, to: usize) {
        let mut order = self.sidebar_order();
        if let Some(from) = order.iter().position(|s| *s == section) {
            order.remove(from);
            order.insert(to.min(order.len()), section);
        }
        self.sidebar_order = order;
    }

    pub fn is_section_collapsed(&self, section: SidebarSection) -> bool {
        self.collapsed_sections.contains(&section)
    }

    pub fn set_section_collapsed(&mut self, section: SidebarSection, collapsed: bool) {
        self.collapsed_sections.retain(|s| *s != section);
        if collapsed {
            self.collapsed_sections.push(section);
        }
    }

    /// Displays the settings dialog: a header, the pages on the left and
    /// the open page on the right, at a fixed size.
    pub(crate) fn display(
        &mut self,
        ctx: &egui::Context,
        plugins: &mut crate::plugins::PluginHost,
    ) {
        let size = dialog_size(ctx);
        let narrow = size.x < NARROW_WIDTH;
        let palette = Palette::for_context(ctx, ctx.global_style().visuals.dark_mode);
        let frame = egui::Frame::popup(&ctx.global_style())
            .fill(palette.sidebar)
            .inner_margin(0);
        let modal = Modal::new("Settings".into()).frame(frame).show(ctx, |ui| {
            ui.set_min_size(size);
            ui.set_max_size(size);
            self.header(ui);
            if narrow {
                self.tabs(ui);
            }
            let body = ui.available_rect_before_wrap();
            let page = if narrow {
                body
            } else {
                let nav = egui::Rect::from_min_max(
                    body.min,
                    egui::pos2(body.left() + NAV_WIDTH, body.bottom()),
                );
                let mut nav_ui = ui.new_child(
                    egui::UiBuilder::new()
                        .max_rect(nav.shrink2(vec2(8.0, 4.0)))
                        .layout(egui::Layout::top_down(egui::Align::Min)),
                );
                self.nav(&mut nav_ui);
                egui::Rect::from_min_max(egui::pos2(nav.right(), body.top()), body.max)
            };
            let radius = ui::card_radius(ui);
            ui.painter().rect_filled(
                page,
                egui::CornerRadius {
                    nw: if narrow { 0 } else { radius },
                    ne: 0,
                    sw: if narrow { radius } else { 0 },
                    se: radius,
                },
                palette.content,
            );
            let mut page_ui = ui.new_child(
                egui::UiBuilder::new()
                    .max_rect(page)
                    .layout(egui::Layout::top_down(egui::Align::Min)),
            );
            self.page(&mut page_ui, plugins);
            ui.allocate_rect(body, egui::Sense::hover());
        });
        self.appearance.apply(ctx);
        if modal.should_close() {
            ActionToPerform::CloseActiveModalWindow.schedule();
        }
    }

    /// "Settings" and the close button.
    #[allow(clippy::unused_self)]
    fn header(&self, ui: &mut Ui) {
        ui.allocate_ui_with_layout(
            vec2(ui.available_width(), 52.0),
            egui::Layout::left_to_right(egui::Align::Center),
            |ui| {
                ui.add_space(20.0);
                ui.label(RichText::new("Settings").size(20.0).strong());
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.add_space(10.0);
                    if ui::icon_button(ui, Icon::X, "Close (Esc)", false).clicked() {
                        ActionToPerform::CloseActiveModalWindow.schedule();
                    }
                });
            },
        );
    }

    /// The list of pages. ↑ and ↓ move between them.
    fn nav(&mut self, ui: &mut Ui) {
        let mut rows = Vec::with_capacity(SettingsCategory::ALL.len());
        for category in SettingsCategory::ALL {
            let row = ui::nav_row(
                ui,
                category.icon(),
                category.label(),
                self.category == category,
                egui::Sense::click(),
            );
            if row.clicked() {
                self.category = category;
            }
            rows.push(row);
        }
        let focused = rows.iter().position(Response::has_focus);
        if let Some(index) = focused {
            let step = ui.input_mut(|i| {
                if i.consume_key(egui::Modifiers::NONE, egui::Key::ArrowDown) {
                    Some(index + 1)
                } else if i.consume_key(egui::Modifiers::NONE, egui::Key::ArrowUp) {
                    index.checked_sub(1)
                } else {
                    None
                }
            });
            if let Some(next) = step.filter(|&next| next < rows.len()) {
                self.category = SettingsCategory::ALL[next];
                rows[next].request_focus();
            }
        }
    }

    /// The pages as tabs, for narrow windows.
    fn tabs(&mut self, ui: &mut Ui) {
        ui.horizontal(|ui| {
            ui.add_space(16.0);
            let choices: Vec<(&str, bool, &str)> = SettingsCategory::ALL
                .iter()
                .map(|category| (category.label(), true, ""))
                .collect();
            let selected = SettingsCategory::ALL
                .iter()
                .position(|category| *category == self.category)
                .unwrap_or_default();
            if let Some(index) = ui::segmented_text(ui, &choices, selected) {
                self.category = SettingsCategory::ALL[index];
            }
        });
        ui.add_space(8.0);
    }

    /// The open page: its title and settings, scrolling as one.
    fn page(&mut self, ui: &mut Ui, plugins: &mut crate::plugins::PluginHost) {
        egui::ScrollArea::vertical()
            .id_salt("settings_page")
            .auto_shrink([false; 2])
            .show(ui, |ui| {
                egui::Frame::new()
                    .inner_margin(egui::Margin {
                        left: 24,
                        right: 24,
                        top: 18,
                        bottom: 20,
                    })
                    .show(ui, |ui| {
                        ui.set_width(ui.available_width());
                        ui.label(RichText::new(self.category.label()).size(18.0).strong());
                        match self.category {
                            SettingsCategory::General => self.general_page(ui),
                            SettingsCategory::Appearance => self.appearance_page(ui),
                            SettingsCategory::Files => self.files_page(ui),
                            SettingsCategory::Previews => self.previews_page(ui),
                            SettingsCategory::Keyboard => crate::app::keymap::shortcut_rows(ui),
                            SettingsCategory::Plugins => {
                                crate::plugins::ui::settings_page(ui, plugins, &mut self.plugins);
                            }
                        }
                        if !matches!(
                            self.category,
                            SettingsCategory::Keyboard | SettingsCategory::Plugins
                        ) {
                            self.restore_button(ui);
                        }
                    });
            });
    }

    fn general_page(&mut self, ui: &mut Ui) {
        let found = cfg!(target_os = "macos") || Path::new(&self.terminal_path).is_file();
        ui::settings_group(ui, "Apps", |group| {
            group.row(
                "Terminal",
                Some(if found {
                    "Opened by \u{201c}Open in terminal\u{201d}"
                } else {
                    "Program not found: check the path"
                }),
                |ui| {
                    #[cfg(any(windows, target_os = "macos"))]
                    if ui.button("Browse…").clicked() {
                        let start = Path::new(&self.terminal_path).parent();
                        if let Some(program) =
                            crate::shell::pick_program(start.filter(|p| p.is_dir()))
                        {
                            self.terminal_path = program.display().to_string();
                        }
                    }
                    ui.add(ui::text_field(ui, &mut self.terminal_path).desired_width(240.0));
                },
            );
        });
        #[cfg(target_os = "macos")]
        ui::settings_group(ui, "Privacy", |group| {
            group.row(
                "Full Disk Access",
                Some(if crate::shell::has_full_disk_access() {
                    "On: every folder opens without asking"
                } else {
                    "Off: macOS asks before DirFleet opens Desktop, Documents, Downloads and drives"
                }),
                |ui| {
                    if ui.button("Open System Settings").clicked() {
                        crate::shell::open_full_disk_access_settings();
                    }
                },
            );
            group.row(
                "Suggest at launch",
                Some("Show a banner while Full Disk Access is off"),
                |ui| {
                    ui::toggle_switch(ui, &mut self.ask_full_disk_access, "Suggest at launch");
                },
            );
        });
        ui::settings_group(ui, "About", |group| {
            group.row(
                "DirFleet",
                Some(concat!("Version ", env!("CARGO_PKG_VERSION"))),
                |ui| {
                    ui::icon_button(ui, Icon::Info, crate::consts::GIT_HASH_INFO, false);
                    ui.hyperlink_to("Project website", crate::consts::HOMEPAGE);
                },
            );
        });
    }

    fn appearance_page(&mut self, ui: &mut Ui) {
        ui::settings_group(ui, "Theme", |group| {
            group.row(
                "App theme",
                Some("Light, dark, or the same as the system"),
                |ui| {
                    choice(
                        ui,
                        "appearance",
                        &mut self.appearance,
                        &Appearance::ALL,
                        Appearance::label,
                    );
                },
            );
        });
        ui::settings_group(ui, "Layout", |group| {
            let trash = format!("{} in the sidebar", trash_bin::LABEL);
            for (title, description, value) in [
                (
                    "Sidebar",
                    "Favorites, drives and recent folders",
                    &mut self.sidebar_visible,
                ),
                (
                    "File inspector",
                    "Details and a preview of the selected item",
                    &mut self.inspector_visible,
                ),
                (
                    "Recent folders in the sidebar",
                    "The folders you opened last",
                    &mut self.show_recent_folders,
                ),
                (
                    trash.as_str(),
                    "Deleted items, ready to restore",
                    &mut self.show_trash,
                ),
                (
                    "Drive capacity",
                    "A bar under each drive showing how full it is",
                    &mut self.show_drive_capacity,
                ),
                (
                    "Drive notifications",
                    "Say when a USB drive or card is connected or removed",
                    &mut self.notify_drive_changes,
                ),
            ] {
                group.row(title, Some(description), |ui| {
                    ui::toggle_switch(ui, value, title);
                });
            }
            group.note("Side panels collapse automatically in smaller windows.");
        });
    }

    fn files_page(&mut self, ui: &mut Ui) {
        let mut view = ui
            .data_get_persisted::<DirectoryViewSettings>()
            .unwrap_or_default();
        let mut hidden = ui
            .data_get_persisted::<DirectoryShowHidden>()
            .unwrap_or_default();
        let before = (view.sorting, view.invert_sort, hidden.0, view.display_type);
        ui::settings_group(ui, "New folders open with", |group| {
            group.row(
                "View",
                Some("Until you choose a view for the folder"),
                |ui| {
                    let grid = usize::from(view.display_type == DisplayType::Icons);
                    let choices = [("List", true, ""), ("Grid", true, "")];
                    if let Some(index) = ui::segmented_text(ui, &choices, grid) {
                        view.display_type = if index == 0 {
                            DisplayType::List
                        } else {
                            DisplayType::Icons
                        };
                    }
                },
            );
            group.row("Sort by", None, |ui| {
                choice(
                    ui,
                    "default_sort",
                    &mut view.sorting,
                    &Sort::ALL,
                    Sort::label,
                );
            });
            group.row("Reverse order", None, |ui| {
                ui.add_enabled_ui(view.sorting != Sort::Random, |ui| {
                    ui::toggle_switch(ui, &mut view.invert_sort, "Reverse order");
                });
            });
            group.row(
                "Hidden items",
                Some("Show files and folders marked as hidden"),
                |ui| {
                    ui::toggle_switch(ui, &mut hidden.0, "Hidden items");
                },
            );
        });
        apply_directory_defaults(ui, view, hidden, before);
        ui::settings_group(ui, "Confirmations", |group| {
            let title = format!("Ask before moving items to the {}", trash_bin::LABEL);
            group.row(
                &title,
                Some("Shift+Delete always asks, because it can't be undone."),
                |ui| {
                    ui::toggle_switch(ui, &mut self.confirm_trash, &title);
                },
            );
            let title = "Ask before moving or copying dragged items";
            group.row(
                title,
                Some("So an item let go over the wrong folder stays where it was."),
                |ui| {
                    ui::toggle_switch(ui, &mut self.confirm_drop, title);
                },
            );
        });
        let now = ui.input(|i| i.time);
        let armed = self
            .clear_history_armed
            .is_some_and(|at| now - at < RESTORE_CONFIRM_SECONDS);
        ui::settings_group(ui, "Open history", |group| {
            let title = "Remember opened files";
            group.row(
                title,
                Some("Files opened here sort by \"Date opened\" and \"Times opened\""),
                |ui| {
                    ui::toggle_switch(ui, &mut self.track_open_history, title);
                },
            );
            group.row("Forget every opened file", None, |ui| {
                let label = if armed {
                    "Click again to clear"
                } else {
                    "Clear open history"
                };
                if ui.button(label).clicked() {
                    if armed {
                        crate::app::open_history::clear();
                        self.clear_history_armed = None;
                        crate::toast!(Info, "Open history cleared");
                    } else {
                        self.clear_history_armed = Some(now);
                    }
                }
            });
        });
        if armed {
            ui.ctx()
                .request_repaint_after(std::time::Duration::from_millis(250));
        }
    }

    fn previews_page(&mut self, ui: &mut Ui) {
        ui::settings_group(ui, "Thumbnails", |group| {
            group.row(
                "Icon size",
                Some("Size of icons and thumbnails in the grid"),
                |ui| {
                    choice(
                        ui,
                        "icon_size",
                        &mut self.icon_size,
                        &IconSize::ALL,
                        IconSize::label,
                    );
                },
            );
            group.row(
                "Animate selected videos and GIFs",
                Some("Quick Look and the inspector always animate."),
                |ui| {
                    ui::toggle_switch(
                        ui,
                        &mut self.animate_selected_previews,
                        "Animate selected videos and GIFs",
                    );
                },
            );
            group.row(
                "Chosen video thumbnails",
                Some("Frames picked with \"Select thumbnail…\""),
                |ui| {
                    if ui.button("Reset all").clicked() {
                        crate::app::thumbnail_choice::clear_all();
                        crate::toast!(Info, "Video thumbnails reset");
                    }
                },
            );
        });
    }

    /// "Restore defaults" for the open page. The first click asks for a
    /// second one, since settings have no undo.
    fn restore_button(&mut self, ui: &mut Ui) {
        let now = ui.input(|i| i.time);
        let armed = self.restore_armed.is_some_and(|(category, at)| {
            category == self.category && now - at < RESTORE_CONFIRM_SECONDS
        });
        ui.add_space(16.0);
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Min), |ui| {
            let label = if armed {
                "Click again to restore"
            } else {
                "Restore defaults"
            };
            let button = egui::Button::new((ui::glyph(Icon::RotateCcw).size(13.0), label));
            let response = ui.add(button);
            response
                .widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, true, label));
            if response
                .on_hover_text("Reset the settings on this page")
                .clicked()
            {
                if armed {
                    self.restore_page(ui);
                    self.restore_armed = None;
                } else {
                    self.restore_armed = Some((self.category, now));
                }
            }
        });
        if armed {
            ui.ctx()
                .request_repaint_after(std::time::Duration::from_millis(250));
        }
    }

    /// Puts the open page's settings back to their defaults.
    fn restore_page(&mut self, ui: &Ui) {
        let defaults = Self::default();
        match self.category {
            SettingsCategory::General => {
                self.terminal_path = defaults.terminal_path;
                self.ask_full_disk_access = defaults.ask_full_disk_access;
            }
            SettingsCategory::Appearance => {
                self.appearance = defaults.appearance;
                self.sidebar_visible = defaults.sidebar_visible;
                self.inspector_visible = defaults.inspector_visible;
                self.show_recent_folders = defaults.show_recent_folders;
                self.show_trash = defaults.show_trash;
                self.show_drive_capacity = defaults.show_drive_capacity;
                self.notify_drive_changes = defaults.notify_drive_changes;
                self.sidebar_order = defaults.sidebar_order;
                self.collapsed_sections = defaults.collapsed_sections;
            }
            SettingsCategory::Files => {
                self.confirm_trash = defaults.confirm_trash;
                self.confirm_drop = defaults.confirm_drop;
                self.track_open_history = defaults.track_open_history;
                ui.data_set_persisted(DirectoryViewSettings::default());
                ui.data_set_persisted(DirectoryShowHidden::default());
                ActionToPerform::ViewSettingsChanged(crate::app::DataSource::Settings).schedule();
            }
            SettingsCategory::Previews => {
                self.icon_size = defaults.icon_size;
                self.animate_selected_previews = defaults.animate_selected_previews;
            }
            SettingsCategory::Keyboard | SettingsCategory::Plugins => return,
        }
        crate::toast!(Info, "{} settings restored", self.category.label());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn old_settings_receive_new_defaults() {
        let settings: ApplicationSettings = serde_json::from_str(
            r#"{"terminal_path":"Terminal","animate_selected_previews":true}"#,
        )
        .expect("legacy settings");
        assert_eq!(settings.appearance, Appearance::System);
        assert!(settings.sidebar_visible);
        assert!(!settings.inspector_visible);
        assert!(settings.show_recent_folders);
        assert!(settings.show_trash);
        assert!(settings.track_open_history);
        assert!((settings.sidebar_width - 200.0).abs() < f32::EPSILON);
    }
    #[test]
    fn panel_preferences_round_trip() {
        let settings = ApplicationSettings {
            appearance: Appearance::Light,
            inspector_visible: true,
            sidebar_width: 225.0,
            track_open_history: false,
            ..Default::default()
        };
        let encoded = serde_json::to_string(&settings).expect("encode");
        let restored: ApplicationSettings = serde_json::from_str(&encoded).expect("decode");
        assert_eq!(restored.appearance, Appearance::Light);
        assert!(restored.inspector_visible);
        assert!((restored.sidebar_width - 225.0).abs() < f32::EPSILON);
        assert!(!restored.track_open_history);
    }
    #[test]
    fn sidebar_sections_default_and_round_trip() {
        let mut settings = ApplicationSettings::default();
        assert_eq!(settings.sidebar_order(), SidebarSection::ALL);
        assert!(!settings.is_section_collapsed(SidebarSection::Drives));
        settings.move_sidebar_section(SidebarSection::Drives, 0);
        settings.set_section_collapsed(SidebarSection::Recent, true);
        let encoded = serde_json::to_string(&settings).expect("encode");
        let restored: ApplicationSettings = serde_json::from_str(&encoded).expect("decode");
        assert_eq!(restored.sidebar_order()[0], SidebarSection::Drives);
        assert!(restored.is_section_collapsed(SidebarSection::Recent));
        let mut restored = restored;
        restored.set_section_collapsed(SidebarSection::Recent, false);
        assert!(!restored.is_section_collapsed(SidebarSection::Recent));
    }
    #[test]
    fn sidebar_order_repairs_partial_lists() {
        let settings = ApplicationSettings {
            sidebar_order: vec![
                SidebarSection::Drives,
                SidebarSection::Drives,
                SidebarSection::Recent,
            ],
            ..Default::default()
        };
        assert_eq!(
            settings.sidebar_order(),
            [
                SidebarSection::Drives,
                SidebarSection::Recent,
                SidebarSection::Favorites,
                SidebarSection::SavedSearches,
                SidebarSection::Locations,
            ]
        );
    }
}
