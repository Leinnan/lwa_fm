use egui::text::LayoutJob;
use egui::{Color32, Layout, TextFormat, Ui};
use egui_dock::{DockArea, DockState, NodeIndex, Style, SurfaceIndex, TabViewer};
use icu::collator::options::{AlternateHandling, CollatorOptions, Strength};
use icu::collator::{Collator, CollatorBorrowed};
use icu::locale::Locale;
use std::borrow::Cow;
use std::fs;
use std::os::unix::ffi::OsStrExt;
use std::path::Path;
use std::str::FromStr;
use std::{ffi::OsStr, path::PathBuf};

use egui::Vec2;
use egui_extras::{Column, TableBuilder};

use crate::helper::KeyWithCommandPressed;
use crate::watcher::DirectoryWatcher;
use crate::{consts::VERTICAL_SPACING, toast};

use super::commands::ActionToPerform;
use super::directory_path_info::DirectoryPathInfo;
use super::directory_view_settings::DirectoryViewSettings;

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub enum EntryType {
    File,
    Directory,
}

#[derive(Debug, Clone)]
pub struct DirEntry {
    pub path: Cow<'static, str>,
    pub entry_type: EntryType,
    pub created_at: u128,
    pub modified_at: u128,
    pub size: u64,
    file_name_index: usize,
}

impl TryFrom<walkdir::DirEntry> for DirEntry {
    type Error = ();

    fn try_from(value: walkdir::DirEntry) -> Result<Self, Self::Error> {
        let Ok(meta) = value.metadata() else {
            return Err(());
        };
        let file_type = meta.file_type();
        #[cfg(target_os = "windows")]
        let is_dir = {
            use std::os::windows::fs::FileTypeExt;
            file_type.is_dir() || file_type.is_symlink_dir()
        };
        #[cfg(not(target_os = "windows"))]
        let is_dir = file_type.is_dir();
        let entry_type = if is_dir {
            EntryType::Directory
        } else {
            EntryType::File
        };
        let path: Cow<'static, str> = Cow::Owned(value.path().to_string_lossy().to_string());
        let file_name_index = path.len() - value.file_name().len();
        let created_at = meta
            .created()
            .map(|time| {
                time.duration_since(std::time::SystemTime::UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_millis()
            })
            .unwrap_or_default();
        let modified_at = meta
            .modified()
            .map(|time| {
                time.duration_since(std::time::SystemTime::UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_millis()
            })
            .unwrap_or_default();
        #[cfg(windows)]
        let size = std::os::windows::fs::MetadataExt::file_size(&meta);
        #[cfg(target_os = "linux")]
        let size = std::os::linux::fs::MetadataExt::st_size(&meta);
        #[cfg(target_os = "macos")]
        let size = std::os::macos::fs::MetadataExt::st_size(&meta);
        Ok(Self {
            path,
            entry_type,
            created_at,
            modified_at,
            size,
            file_name_index,
        })
    }
}
impl DirEntry {
    pub fn get_path(&self) -> &Path {
        Path::new(OsStr::from_bytes(self.path.as_bytes()))
    }
    pub fn is_file(&self) -> bool {
        self.entry_type == EntryType::File
    }

    pub fn get_splitted_path(&self) -> (&str, &str) {
        self.path.split_at(self.file_name_index)
    }
}

pub type TabPaths = Vec<PathBuf>;

#[derive(Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum CurrentPath {
    None,
    One(PathBuf),
    Multiple(Vec<PathBuf>),
}

impl From<PathBuf> for CurrentPath {
    fn from(path: PathBuf) -> Self {
        if path.is_dir() {
            Self::One(path)
        } else {
            Self::None
        }
    }
}

impl CurrentPath {
    pub fn get_name_from_path(&self) -> String {
        match &self {
            Self::None => "Empty".into(),
            Self::One(path) => {
                #[cfg(not(windows))]
                {
                    path.iter()
                        .next_back()
                        .expect("FAILED")
                        .to_string_lossy()
                        .into_owned()
                }
                #[cfg(windows)]
                {
                    let mut result = path
                        .iter()
                        .last()
                        .expect("FAILED")
                        .to_string_lossy()
                        .into_owned();
                    if result.len() == 1 {
                        result = path.display().to_string();
                    }
                    result
                }
            }
            Self::Multiple(_) => "Multiple".into(),
        }
    }
    pub fn parent(&self) -> Option<PathBuf> {
        match self {
            Self::One(path) => path.parent().map(PathBuf::from),
            _ => None,
        }
    }
    pub fn single_path(&self) -> Option<PathBuf> {
        match self {
            Self::One(path) => Some(path.clone()),
            _ => None,
        }
    }

    pub const fn multiple_paths(&self) -> bool {
        matches!(self, Self::Multiple(_))
    }
}

#[derive(Debug)]
pub struct TabData {
    pub list: Vec<DirEntry>,
    pub action_to_perform: Option<ActionToPerform>,
    pub current_path: CurrentPath,
    pub settings: DirectoryViewSettings,
    pub can_close: bool,
    pub search: super::Search,
    pub path_info: DirectoryPathInfo,
    pub collator: CollatorBorrowed<'static>,
    pub watcher: Option<DirectoryWatcher>,
}

pub fn build_collator(case_sensitive: bool) -> CollatorBorrowed<'static> {
    let mut options = CollatorOptions::default();
    options.strength = if case_sensitive {
        Some(Strength::Tertiary)
    } else {
        Some(Strength::Primary)
    };
    options.alternate_handling = Some(AlternateHandling::Shifted);
    let lang = bevy_device_lang::get_lang().unwrap_or("en".to_string());
    let prefs = Locale::try_from_str(&lang).unwrap_or(Locale::try_from_str("en").unwrap());
    Collator::try_new(prefs.into(), options).unwrap()
}

impl TabData {
    pub const fn is_searching(&self) -> bool {
        !self.search.value.is_empty()
    }
    pub fn from_path(path: &PathBuf) -> Self {
        let path_info = DirectoryPathInfo::default();
        let mut new = Self {
            list: vec![],
            collator: build_collator(false),
            action_to_perform: None,
            current_path: CurrentPath::None,
            settings: DirectoryViewSettings::default(),
            can_close: true,
            path_info,
            search: super::Search {
                visible: false,
                case_sensitive: false,
                depth: 3,
                value: String::new(),
            },
            watcher: DirectoryWatcher::new().ok(),
        };
        new.set_path(CurrentPath::One(path.clone()));
        new
    }
    pub fn update(&mut self, is_only_tab: bool) {
        self.can_close = !is_only_tab;
        self.action_to_perform = None;
        // self.other_tabs_paths = other_tabs
        //     .iter()
        //     .filter(|p| !p.eq(&&self.current_path))
        //     .cloned()
        //     .collect::<Vec<PathBuf>>();
        // Check for file system events
        self.check_for_file_system_events();
    }

    pub(crate) const fn toggle_top_edit(&mut self) {
        self.path_info.editable = !self.is_searching() && !self.path_info.editable;
    }
}

pub struct MyTabViewer;

impl TabViewer for MyTabViewer {
    // This associated type is used to attach some data to each tab.
    type Tab = TabData;

    fn allowed_in_windows(&self, _tab: &mut Self::Tab) -> bool {
        false
    }

    fn closeable(&mut self, tab: &mut Self::Tab) -> bool {
        tab.can_close
    }

    // Returns the current `tab`'s title.
    fn title(&mut self, tab: &mut Self::Tab) -> egui_dock::egui::WidgetText {
        if tab.is_searching() {
            format!("Searching: {}", &tab.search.value).into()
        } else {
            let path = tab.current_path.get_name_from_path();
            path.into()
        }
    }

    /// Defines the contents of a given `tab`.
    #[allow(clippy::too_many_lines)]
    fn ui(&mut self, ui: &mut Ui, tab: &mut Self::Tab) {
        let cmd = ui.command_pressed();
        if ui.key_with_command_pressed(egui::Key::F) {
            tab.search.visible = !tab.search.visible;
        }
        if ui.key_with_command_pressed(egui::Key::H) {
            tab.settings.show_hidden = !tab.settings.show_hidden;
            tab.refresh_list();
        }
        if ui.key_with_command_pressed(egui::Key::ArrowUp) && !tab.is_searching() {
            let Some(parent) = tab.current_path.parent() else {
                return;
            };
            tab.action_to_perform = Some(ActionToPerform::ChangePath(parent));
            return;
        }
        let mut require_refresh = false; //  replace it with flow using https://docs.rs/notify/latest/notify/

        let favorites_paths: Vec<Cow<'static, str>> = ui
            .data(|d| d.get_temp("FavoritesPaths".into()))
            .unwrap_or_default();
        if tab.search.visible {
            ui.with_layout(Layout::right_to_left(eframe::emath::Align::Min), |ui| {
                let search_input =
                    ui.add(egui::TextEdit::singleline(&mut tab.search.value).hint_text("Search"));
                require_refresh |= search_input.changed();
                ui.memory_mut(|memory| {
                    memory.request_focus(search_input.id);
                });
                require_refresh |= ui
                    .add(egui::Slider::new(&mut tab.search.depth, 1..=7))
                    .on_hover_text("Search depth")
                    .changed();
                require_refresh |= ui
                    .toggle_value(&mut tab.search.case_sensitive, "🇨")
                    .on_hover_text("Case sensitive")
                    .changed();
                let mut was_favorites = tab.current_path.multiple_paths();
                let favorites_changed = ui
                    .toggle_value(&mut was_favorites, "💕")
                    .on_hover_text("Search favorites")
                    .changed();
                if favorites_changed {
                    tab.action_to_perform = Some(ActionToPerform::SearchInFavorites(was_favorites));
                    return;
                }

                require_refresh |= favorites_changed;
            });
            ui.add_space(crate::consts::TOP_SIDE_MARGIN);
        }
        let is_searching = tab.is_searching();
        let multiple_dirs = tab.current_path.multiple_paths();
        let text_height = egui::TextStyle::Body.resolve(ui.style()).size * 2.0;
        let table = TableBuilder::new(ui)
            .striped(true)
            .vscroll(false)
            .cell_layout(egui::Layout::left_to_right(egui::Align::Center))
            .column(Column::remainder().at_least(260.0))
            .resizable(false);
        table.body(|body| {
            body.rows(text_height, tab.list.len(), |mut row| {
                let val = &tab.list[row.index()];
                // let Ok(meta) = val.metadata() else {
                //     return;
                // };
                let indexed = if cmd { row.index() + 1 } else { 11 };
                row.col(|ui| {
                    ui.add_space(VERTICAL_SPACING);
                    // ui.allocate_space(egui::vec2(available_width, 20.0));
                    // let file_type = meta.file_type();
                    // #[cfg(target_os = "windows")]
                    // let is_dir = {
                    //     use std::os::windows::fs::FileTypeExt;
                    //     file_type.is_dir() || file_type.is_symlink_dir()
                    // };
                    // #[cfg(not(target_os = "windows"))]
                    // let is_dir = file_type.is_dir();
                    let is_dir = val.entry_type == EntryType::Directory;
                    let (dir, file) = val.get_splitted_path();
                    let mut s = LayoutJob::default();
                    let file_format = if is_dir {
                        TextFormat {
                            color: Color32::LIGHT_GRAY,
                            ..Default::default()
                        }
                    } else {
                        TextFormat {
                            color: Color32::GRAY,
                            ..Default::default()
                        }
                    };
                    if indexed < 10 {
                        s.append(&format!("[{indexed}] "), 0.0, TextFormat::default());
                    }
                    s.append(file, 0.0, file_format);
                    if is_searching || multiple_dirs {
                        s.append(
                            dir,
                            10.0,
                            TextFormat {
                                color: Color32::DARK_GRAY,
                                ..Default::default()
                            },
                        );
                    }
                    if !is_dir {
                        let size_text = crate::helper::format_bytes_simple(val.size);
                        s.append(
                            &size_text,
                            10.0,
                            TextFormat {
                                color: Color32::DARK_GRAY,
                                ..Default::default()
                            },
                        );
                    }
                    let added_button =
                        ui.add(egui::Button::new(s).fill(egui::Color32::from_white_alpha(0)));

                    if added_button.clicked()
                        || convert_nr_to_egui_key(indexed)
                            .is_some_and(|k| ui.key_with_command_pressed(k))
                    {
                        if val.entry_type == EntryType::File {
                            let _ = open::that_detached(val.get_path());
                        } else {
                            let Ok(path) = std::fs::canonicalize(val.get_path()) else {
                                return;
                            };
                            let action = if ui.shift_pressed() {
                                ActionToPerform::NewTab(path)
                            } else {
                                ActionToPerform::ChangePath(path)
                            };
                            tab.action_to_perform = action.into();
                        }
                    }
                    added_button.context_menu(|ui| {
                        if is_dir {
                            if ui.button("Open").clicked() {
                                tab.action_to_perform = ActionToPerform::ChangePath(
                                    PathBuf::from_str(&val.path).expect("msg"),
                                )
                                .into();
                                ui.close_menu();
                                return;
                            }
                            if ui.button("Open in new tab").clicked() {
                                tab.action_to_perform = ActionToPerform::NewTab(
                                    PathBuf::from_str(&val.path).expect("msg"),
                                )
                                .into();
                                ui.close_menu();
                                return;
                            }
                            #[cfg(windows)]
                            let open_name = "Open in Explorer";
                            #[cfg(target_os = "macos")]
                            let open_name = "Open in Finder";
                            #[cfg(target_os = "linux")]
                            let open_name = "Open in File Manager";

                            if ui.button(open_name).clicked() {
                                open::that_detached(val.get_path())
                                    .expect("Failed to open directory in file manager");
                                ui.close_menu();
                                return;
                            }

                            let exist_in_favorites =
                                favorites_paths.iter().any(|f| f.eq(&val.path));

                            if !exist_in_favorites && ui.button("Add to favorites").clicked() {
                                tab.action_to_perform =
                                    Some(ActionToPerform::AddToFavorites(val.path.clone()));

                                ui.close_menu();
                                return;
                            }

                            if exist_in_favorites && ui.button("Remove from favorites").clicked() {
                                tab.action_to_perform =
                                    Some(ActionToPerform::RemoveFromFavorites(val.path.clone()));
                                ui.close_menu();
                            }
                        } else {
                            if ui.button("Open").clicked() {
                                let _ = open::that_detached(val.get_path());
                                ui.close_menu();
                                return;
                            }
                            #[cfg(windows)]
                            if ui.button("Show in explorer").clicked() {
                                crate::windows_tools::display_in_explorer(val.path())
                                    .unwrap_or_else(|_| {
                                        toast!(Error, "Could not open in explorer");
                                    });
                                ui.close_menu();
                            }
                            let tab_paths = ui
                                .data::<Option<TabPaths>>(|data| data.get_temp("TabPaths".into()))
                                .unwrap_or_default();
                            #[allow(clippy::collapsible_else_if)]
                            if tab_paths.len() > 1 {
                                ui.separator();
                                ui.menu_button("Move to", |ui| {
                                    for other in &tab_paths {
                                        if other == val.get_path() {
                                            continue;
                                        }
                                        let other = PathBuf::from(
                                            std::fs::canonicalize(other)
                                                .unwrap_or_else(|_| val.get_path().to_path_buf())
                                                .to_string_lossy()
                                                .replace("\\\\?\\", ""),
                                        );

                                        if ui
                                            .button(format!(
                                                "{}",
                                                &other
                                                    .file_name()
                                                    .expect("Failed")
                                                    .to_string_lossy()
                                            ))
                                            .on_hover_text(other.display().to_string())
                                            .clicked()
                                        {
                                            let path = val.get_path();

                                            let filename = path.file_name().expect("NO FILENAME");
                                            let target_path = other.join(filename);
                                            println!("{}", &target_path.display());
                                            let move_result = fs::rename(path, &target_path);
                                            let mut success = move_result.is_ok();
                                            let _ = move_result.inspect_err(|e| {
                                                e.raw_os_error().inspect(|nr| {
                                                    // OSError: [WinError 17] The system cannot move the file to a different disk drive
                                                    if *nr == 17 {
                                                        let copy_success =
                                                            fs::copy(path, target_path.clone())
                                                                .is_ok();
                                                        if copy_success {
                                                            success = fs::remove_file(path).is_ok();
                                                        }
                                                    }
                                                });
                                            });
                                            if !success {
                                                toast!(
                                                    Error,
                                                    "Failed to move file {}",
                                                    filename.to_string_lossy()
                                                );
                                            }
                                            require_refresh = true;
                                            ui.close_menu();
                                        }
                                    }
                                });
                            }
                        }
                        ui.separator();
                        if ui.button("Move to Trash").clicked() {
                            trash::delete(val.get_path()).unwrap_or_else(|_| {
                                toast!(Error, "Could not move it to trash.");
                            });
                            ui.close_menu();
                            require_refresh = true;
                        }
                        if ui.button("Copy path to clipboard").clicked() {
                            let Ok(mut clipboard) = arboard::Clipboard::new() else {
                                toast!(Error, "Failed to read the clipboard.");
                                return;
                            };
                            let path = val.get_path().display().to_string();
                            clipboard.set_text(path).unwrap_or_else(|_| {
                                toast!(Error, "Failed to update the clipboard.");
                            });
                            ui.close_menu();
                        }

                        #[cfg(windows)]
                        {
                            ui.separator();
                            if ui.button("Properties").clicked() {
                                crate::windows_tools::open_properties(val.path());
                                ui.close_menu();
                            }
                        }
                    });
                    let ext = val
                        .get_path()
                        .extension()
                        .unwrap_or_default()
                        .to_ascii_lowercase();
                    if ext.eq(&OsStr::new("png"))
                        || ext.eq(&OsStr::new("jpg"))
                        || ext.eq(&OsStr::new("jpeg"))
                    {
                        added_button.on_hover_ui(|ui| {
                            let path = std::fs::canonicalize(val.get_path())
                                .unwrap_or_else(|_| val.get_path().to_path_buf())
                                .to_string_lossy()
                                .replace("\\\\?\\", "");
                            let path = format!("file://{path}");
                            ui.add(
                                egui::Image::new(path)
                                    .maintain_aspect_ratio(true)
                                    .max_size(Vec2::new(300.0, 300.0)),
                            );
                        });
                    } else {
                        added_button.on_hover_text(format!(
                            "{:?}",
                            // consider caching here
                            std::fs::canonicalize(val.get_path())
                                .unwrap_or_else(|_| val.get_path().to_path_buf())
                        ));
                    }
                });
            });
        });
        if require_refresh {
            tab.refresh_list();
        }
    }
}

// Here is a simple example of how you can manage a `DockState` of your application.
#[derive(Debug)]
pub struct MyTabs {
    dock_state: DockState<TabData>,
}

impl MyTabs {
    pub fn get_current_path(&mut self) -> Option<PathBuf> {
        let Some(active_tab) = self.get_current_tab() else {
            return None;
        };
        active_tab.current_path.single_path()
    }
    pub fn new(path: &PathBuf) -> Self {
        // Create a `DockState` with an initial tab "tab1" in the main `Surface`'s root node.
        let tabs = vec![TabData::from_path(path)];
        let dock_state = DockState::new(tabs);
        Self { dock_state }
    }

    pub fn get_current_tab(&mut self) -> Option<&mut TabData> {
        let length = self.dock_state.iter_all_tabs_mut().count();
        if length == 1 {
            return Some(
                self.dock_state
                    .iter_all_tabs_mut()
                    .next()
                    .expect("Failed to get data")
                    .1,
            );
        }
        let (_, active_tab) = self.dock_state.find_active_focused()?;
        Some(active_tab)
    }

    pub fn update_active_tab(&mut self, path: impl Into<CurrentPath>) {
        let Some(active_tab) = self.get_current_tab() else {
            return;
        };
        active_tab.set_path(path.into());
    }

    pub fn ui(&mut self, ui: &mut Ui) -> Option<ActionToPerform> {
        let tabs = self.get_tabs_paths();
        let tabs_len = tabs.len();
        ui.data_mut(|data| data.insert_temp("TabPaths".into(), tabs));
        for ((_, _), item) in self.dock_state.iter_all_tabs_mut() {
            item.update(tabs_len == 1);
        }
        DockArea::new(&mut self.dock_state)
            .show_leaf_close_all_buttons(false)
            .show_leaf_collapse_buttons(false)
            .style(Self::get_dock_style(ui.style().as_ref(), tabs_len))
            .show_inside(ui, &mut MyTabViewer);
        if let Some(active_tab) = self.get_current_tab() {
            active_tab.action_to_perform.clone()
        } else {
            None
        }
    }

    pub fn open_in_new_tab(&mut self, path: &PathBuf) {
        let is_not_focused = self.dock_state.focused_leaf().is_none();
        if is_not_focused {
            self.dock_state
                .set_focused_node_and_surface((SurfaceIndex::main(), NodeIndex::root()));
        }
        let new_window = TabData::from_path(path);
        let root_node = self
            .dock_state
            .main_surface_mut()
            .root_node_mut()
            .expect("NO ROOT");
        if root_node.is_leaf() {
            root_node.append_tab(new_window);
        } else {
            self.dock_state.push_to_focused_leaf(new_window);
        }
    }

    pub(crate) fn refresh_list(&mut self) {
        let Some(active_tab) = self.get_current_tab() else {
            return;
        };
        active_tab.refresh_list();
    }

    fn get_tabs_paths(&self) -> Vec<PathBuf> {
        self.dock_state
            .iter_all_tabs()
            .filter_map(|(_, tab)| tab.current_path.single_path())
            .collect()
    }

    fn get_dock_style(ui: &egui::Style, tabs_amount: usize) -> Style {
        let mut style = Style::from_egui(ui);
        style.dock_area_padding = None;
        style.tab_bar.fill_tab_bar = true;
        style.tab_bar.height = if tabs_amount > 1 {
            style.tab_bar.height * 1.4
        } else {
            0.0
        };
        style.tab.tab_body.inner_margin = egui::Margin::same(10);
        style.tab.tab_body.stroke = egui::Stroke::NONE;
        style
    }
}

const fn convert_nr_to_egui_key(nr: usize) -> Option<egui::Key> {
    match nr {
        1 => Some(egui::Key::Num1),
        2 => Some(egui::Key::Num2),
        3 => Some(egui::Key::Num3),
        4 => Some(egui::Key::Num4),
        5 => Some(egui::Key::Num5),
        6 => Some(egui::Key::Num6),
        7 => Some(egui::Key::Num7),
        8 => Some(egui::Key::Num8),
        9 => Some(egui::Key::Num9),
        0 => Some(egui::Key::Num0),
        _ => None,
    }
}
