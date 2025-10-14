use egui::text::LayoutJob;
use egui::util::undoer::Undoer;
use egui::{Color32, FontId, Id, Layout, Sense, TextBuffer, TextFormat, Ui};
use egui_dock::{DockArea, DockState, NodeIndex, Style, SurfaceIndex, TabViewer};
use icu::collator::options::{AlternateHandling, CollatorOptions, Strength};
use icu::collator::{Collator, CollatorBorrowed};
use icu::locale::Locale;
use serde::{Deserialize, Serialize};
// use smallvec::SmallVec;
use std::borrow::Cow;
use std::fs;
use std::path::Path;
use std::time::SystemTime;
use std::{ffi::OsStr, path::PathBuf};

use egui::Vec2;
use egui_extras::{Column, TableBuilder};

use crate::app::command_palette::build_for_path;
use crate::app::commands::{ModalWindow, TabAction};
// use crate::app::dir_handling::COLLATER;
use crate::app::directory_view_settings::{DirectoryShowHidden, DirectoryViewSettings};
use crate::app::{Search, Sort};
use crate::helper::{DataHolder, KeyWithCommandPressed, PathFixer};
use crate::locations::Locations;
use crate::toast;
use crate::watcher::DirectoryWatcher;

use super::commands::ActionToPerform;

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub enum EntryType {
    File,
    Directory,
}

#[derive(Debug, Clone)]
pub struct DirEntry {
    pub path: Cow<'static, str>,
    pub entry_type: EntryType,
    pub created_at: TimestampSeconds,
    pub modified_at: TimestampSeconds,
    pub size: u64,
    file_name_index: usize,
    // pub sort_key: SmallVec<[u8; 40]>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Default)]
pub struct TimestampSeconds(u64);

impl From<SystemTime> for TimestampSeconds {
    fn from(value: SystemTime) -> Self {
        // Unix timestamp in seconds (valid until year 2262)
        let timestamp_seconds: u64 = value
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        Self(timestamp_seconds)
    }
}

impl TryFrom<std::fs::DirEntry> for DirEntry {
    type Error = ();

    fn try_from(value: std::fs::DirEntry) -> Result<Self, Self::Error> {
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
        // let mut sort_key = SmallVec::<[u8; 40]>::new();
        // let _ =
        //     COLLATER.write_sort_key_utf8_to(value.file_name().as_encoded_bytes(), &mut sort_key);
        let path: Cow<'static, str> = Cow::Owned(value.path().to_string_lossy().to_string());
        let file_name_index = path.len() - value.file_name().len();
        let created_at = meta
            .created()
            .map(TimestampSeconds::from)
            .unwrap_or_default();
        let modified_at = meta
            .modified()
            .map(TimestampSeconds::from)
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
            // sort_key,
        })
    }
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
        // let mut sort_key = SmallVec::<[u8; 40]>::new();
        // let _ =
        //     COLLATER.write_sort_key_utf8_to(value.file_name().as_encoded_bytes(), &mut sort_key);
        let created_at = meta
            .created()
            .map(TimestampSeconds::from)
            .unwrap_or_default();
        let modified_at = meta
            .modified()
            .map(TimestampSeconds::from)
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
            // sort_key,
        })
    }
}
impl DirEntry {
    pub fn get_path(&self) -> &Path {
        Path::new(self.path.as_str())
    }

    #[inline]
    pub const fn is_file(&self) -> bool {
        matches!(self.entry_type, EntryType::File)
    }

    pub fn get_splitted_path(&self) -> (&str, &str) {
        self.path.split_at(self.file_name_index)
    }
}

#[derive(Debug, PartialEq, Eq, PartialOrd, Ord, Clone, Default, Hash)]
pub enum CurrentPath {
    #[default]
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
    pub current_path: CurrentPath,
    pub show_hidden: bool,
    pub search: Option<Search>,
    pub watcher: Option<DirectoryWatcher>,
    undoer: Undoer<CurrentPath>,
    pub id: u32,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
struct PopupOpened(pub Id, pub usize);

impl Default for PopupOpened {
    fn default() -> Self {
        Self(Id::new(0), usize::MAX)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash, Default)]
pub struct Selected(pub Vec<usize>);

impl TabData {
    pub fn can_undo(&self) -> bool {
        self.undoer.has_undo(&self.current_path)
    }
    pub fn can_redo(&self) -> bool {
        self.undoer.has_redo(&self.current_path)
    }
    pub fn undo(&mut self) -> Option<ActionToPerform> {
        self.undoer.undo(&self.current_path).map(|s| {
            ActionToPerform::TabAction(
                super::commands::TabTarget::TabWithId(self.id),
                TabAction::ChangePaths(s.to_owned()),
            )
        })
    }
    pub fn redo(&mut self) -> Option<ActionToPerform> {
        self.undoer.redo(&self.current_path).map(|s| {
            ActionToPerform::TabAction(
                super::commands::TabTarget::TabWithId(self.id),
                TabAction::ChangePaths(s.to_owned()),
            )
        })
    }
    pub fn toggle_search(&mut self, data_source: &impl DataHolder) {
        if let Some(search) = &self.search {
            data_source.data_set_tab::<Search>(self.id, search.clone());
            self.search = None;
            if self.current_path.multiple_paths() {
                let Some(path) = self
                    .undoer
                    .undo(&self.current_path)
                    .map(std::borrow::ToOwned::to_owned)
                else {
                    return;
                };
                TabAction::ChangePaths(path).schedule_tab(self.id);
                // self.set_path(path, Some(data_source));
            }
        } else {
            self.search = Some(
                data_source
                    .data_get_tab::<Search>(self.id)
                    .unwrap_or_default(),
            );
        }
    }
}

pub fn build_collator(case_sensitive: bool) -> CollatorBorrowed<'static> {
    let mut options = CollatorOptions::default();
    options.strength = if case_sensitive {
        Some(Strength::Tertiary)
    } else {
        Some(Strength::Primary)
    };
    options.alternate_handling = Some(AlternateHandling::Shifted);
    let lang = bevy_device_lang::get_lang().unwrap_or_else(|| "en".to_string());
    let prefs = Locale::try_from_str(&lang)
        .unwrap_or_else(|_| Locale::try_from_str("en").expect("Failed to create default locale"));
    Collator::try_new(prefs.into(), options).expect("Failed to create collator")
}

pub fn get_id() -> u32 {
    use std::sync::atomic::{AtomicU32, Ordering};
    static COUNTER: AtomicU32 = AtomicU32::new(0);
    COUNTER.fetch_add(1, Ordering::Relaxed) + 1
}

impl TabData {
    pub const fn is_searching(&self) -> bool {
        match &self.search {
            Some(Search {
                value,
                depth: _,
                case_sensitive: _,
            }) => !value.is_empty(),
            None => false,
        }
    }
    pub fn from_path(path: &Path) -> Self {
        let new = Self {
            id: get_id(),
            list: vec![],
            current_path: CurrentPath::None,
            show_hidden: false,
            search: None,
            watcher: DirectoryWatcher::new().ok(),
            undoer: Undoer::default(),
        };
        TabAction::ChangePaths(CurrentPath::One(path.into())).schedule_tab(new.id);
        new
    }
}

pub struct MyTabViewer {
    closeable: bool,
    tab_paths: Vec<PathBuf>,
    active_tab: Option<u32>,
}

impl TabViewer for MyTabViewer {
    // This associated type is used to attach some data to each tab.
    type Tab = TabData;

    fn allowed_in_windows(&self, _tab: &mut Self::Tab) -> bool {
        false
    }

    fn closeable(&mut self, _: &mut Self::Tab) -> bool {
        self.closeable
    }

    fn id(&mut self, tab: &mut Self::Tab) -> Id {
        Id::new(tab.id)
    }

    // Returns the current `tab`'s title.
    fn title(&mut self, tab: &mut Self::Tab) -> egui_dock::egui::WidgetText {
        if tab.is_searching() {
            tab.search
                .as_ref()
                .map(|s| format!("Searching: {}", &s.value))
                .unwrap_or_default()
                .into()
        } else {
            let path = tab.current_path.get_name_from_path();
            path.into()
        }
    }
    fn scroll_bars(&self, _tab: &Self::Tab) -> [bool; 2] {
        [false, false]
    }

    /// Defines the contents of a given `tab`.
    #[allow(clippy::too_many_lines)]
    fn ui(&mut self, ui: &mut Ui, tab: &mut Self::Tab) {
        let cmd = ui.command_pressed();
        if !matches!(&tab.current_path, CurrentPath::None) {
            tab.undoer
                .feed_state(ui.ctx().input(|input| input.time), &tab.current_path);
        }
        if ui.key_with_command_pressed(egui::Key::F) {
            tab.toggle_search(ui.ctx());
        }
        if ui.key_with_command_pressed(egui::Key::H) {
            let mut show_hidden =
                ui.data_get_path_or_persisted::<DirectoryShowHidden>(&tab.current_path);
            show_hidden.0 = !show_hidden.0;
            ui.data_set_path(&tab.current_path, show_hidden.data);
            ActionToPerform::ViewSettingsChanged(super::DataSource::Local).schedule();
        }
        if ui.key_with_command_pressed(egui::Key::ArrowUp) && !tab.is_searching() {
            let Some(parent) = tab.current_path.parent() else {
                return;
            };
            TabAction::ChangePaths(parent.into()).schedule_tab(tab.id);
            return;
        }
        let favorites = ui.data_get_persisted::<Locations>().unwrap_or_default();

        let is_searching = tab.is_searching();
        let tab_id = tab.id;
        let multiple_dirs = tab.current_path.multiple_paths();
        let text_height = egui::TextStyle::Body.resolve(ui.style()).size * 1.5;

        let tab_popup_id = Id::new(tab_id).with(1500);
        let mut selected_tabs = ui
            .data_get_path::<Selected>(&tab.current_path)
            .unwrap_or_default();
        let opened_popup = ui
            .ctx()
            .data_get_tab::<PopupOpened>(tab_id)
            .and_then(|d| {
                if egui::Popup::is_id_open(ui.ctx(), d.0) {
                    Some(d.1)
                } else {
                    None
                }
            })
            .unwrap_or(usize::MAX);
        let mut table = TableBuilder::new(ui)
            .striped(true)
            .vscroll(true)
            .id_salt(Id::new(tab.id).with(&tab.current_path))
            .cell_layout(egui::Layout::left_to_right(egui::Align::Center))
            .column(Column::remainder().at_least(260.0))
            .column(Column::auto().at_most(150.0).at_least(100.0))
            .column(Column::auto().at_most(150.0).at_least(100.0))
            .sense(egui::Sense::click());

        if let Some(v) = selected_tabs.0.first()
            && *v < tab.list.len()
        {
            table = table.scroll_to_row(*v, None);
        }
        let mut new_sort = None;

        table
            .header(text_height, |mut row| {
                row.col(|col| {
                    let res = col
                        .vertical_centered_justified(|ui| {
                            ui.add(
                                egui::Label::new("Name")
                                    .wrap_mode(egui::TextWrapMode::Truncate)
                                    .selectable(false)
                                    .sense(Sense::click()),
                            )
                        })
                        .inner;
                    if res.clicked() {
                        new_sort = Some(Sort::Name);
                    }
                });
                row.col(|col| {
                    let res = col
                        .vertical_centered_justified(|ui| {
                            ui.add(
                                egui::Label::new("Modified")
                                    .wrap_mode(egui::TextWrapMode::Truncate)
                                    .selectable(false)
                                    .sense(Sense::click()),
                            )
                        })
                        .inner;
                    if res.clicked() {
                        new_sort = Some(Sort::Modified);
                    }
                });
                row.col(|col| {
                    let res = col
                        .vertical_centered_justified(|ui| {
                            ui.add(
                                egui::Label::new("Size")
                                    .wrap_mode(egui::TextWrapMode::Truncate)
                                    .selectable(false)
                                    .sense(Sense::click()),
                            )
                        })
                        .inner;
                    if res.clicked() {
                        new_sort = Some(Sort::Size);
                    }
                });
            })
            .body(|body| {
                body.rows(text_height, tab.list.len(), |mut row| {
                    let val = &tab.list[row.index()];
                    let is_dir = val.entry_type == EntryType::Directory;
                    let indexed = row.index() + 1;

                    if row.index() == opened_popup {
                        row.set_hovered(true);
                    } else if selected_tabs.0.contains(&row.index()) {
                        row.set_selected(true);
                    }
                    row.col(|ui| {
                        let color = if is_dir {
                            Color32::LIGHT_GRAY
                        } else {
                            Color32::GRAY
                        };
                        if cmd {
                            if indexed < 10 {
                                ui.add_sized(
                                    [25.0, ui.available_height()],
                                    egui::Label::new(LayoutJob::simple_format(
                                        format!("[{indexed}]"),
                                        TextFormat {
                                            color: Color32::DARK_GRAY,
                                            ..Default::default()
                                        },
                                    )),
                                );
                            } else {
                                ui.add_sized([25.0, ui.available_height()], egui::Label::new(""));
                            }
                        }
                        let (dir, file) = val.get_splitted_path();
                        if is_searching && multiple_dirs {
                            ui.add(
                                egui::Label::new(LayoutJob::simple_singleline(
                                    dir.into(),
                                    FontId::default(),
                                    Color32::DARK_GRAY,
                                ))
                                .wrap_mode(egui::TextWrapMode::Truncate)
                                .selectable(false)
                                .sense(Sense::empty()),
                            );
                        }

                        let added_button = ui.add(
                            egui::Label::new(LayoutJob::simple_singleline(
                                file.into(),
                                FontId::default(),
                                color,
                            ))
                            .wrap_mode(egui::TextWrapMode::Truncate)
                            .selectable(false)
                            .sense(Sense::empty()),
                        );

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
                            added_button.on_hover_text(val.path.as_str());
                        }
                    });

                    if row.response().double_clicked()
                        || convert_nr_to_egui_key(indexed)
                            .is_some_and(|k| row.response().ctx.key_with_command_pressed(k))
                    {
                        if val.entry_type == EntryType::File {
                            ActionToPerform::SystemOpen(val.path.clone()).schedule();
                        } else {
                            let Ok(path) = std::fs::canonicalize(val.get_path()) else {
                                return;
                            };
                            if row.response().ctx.shift_pressed() {
                                ActionToPerform::NewTab(path).schedule();
                            } else {
                                TabAction::ChangePaths(path.into()).schedule_tab(tab_id);
                            }
                        }
                    } else if row.response().clicked() {
                        selected_tabs.0.clear();
                        selected_tabs.0.push(row.index());
                        row.response()
                            .ctx
                            .data_set_path(&tab.current_path, selected_tabs.clone());
                    }
                    row.col(|ui| {
                        let time = {
                            let datetime = std::time::UNIX_EPOCH
                                + std::time::Duration::from_secs(val.modified_at.0);
                            let system_time: std::time::SystemTime = datetime;
                            system_time.elapsed().map_or_else(
                                |_| "Future".to_string(),
                                |elapsed| {
                                    let days = elapsed.as_secs() / 86400;
                                    if days > 0 {
                                        format!("{days} days ago")
                                    } else {
                                        let hours = elapsed.as_secs() / 3600;
                                        if hours > 0 {
                                            format!("{hours} hours ago")
                                        } else {
                                            let minutes = elapsed.as_secs() / 60;
                                            if minutes > 0 {
                                                format!("{minutes} min ago")
                                            } else {
                                                "Just now".to_string()
                                            }
                                        }
                                    }
                                },
                            )
                        };
                        ui.with_layout(Layout::right_to_left(egui::Align::Center), |ui| {
                            ui.add(
                                egui::Label::new(LayoutJob::simple_singleline(
                                    time,
                                    FontId::default(),
                                    Color32::DARK_GRAY,
                                ))
                                .wrap_mode(egui::TextWrapMode::Truncate)
                                .selectable(false)
                                .sense(Sense::empty()),
                            );
                        });
                    });
                    row.col(|ui| {
                        let size_text = if is_dir {
                            ui.add_space(1.0);
                            return;
                        } else {
                            crate::helper::format_bytes_simple(val.size)
                        };
                        ui.with_layout(Layout::right_to_left(egui::Align::Center), |ui| {
                            ui.add(
                                egui::Label::new(LayoutJob::simple_singleline(
                                    size_text,
                                    FontId::default(),
                                    Color32::DARK_GRAY,
                                ))
                                .wrap_mode(egui::TextWrapMode::Truncate)
                                .selectable(false)
                                .sense(Sense::empty()),
                            );
                        });
                    });
                    let id = tab_popup_id.with(row.index());
                    egui::Popup::context_menu(&row.response())
                        .id(id)
                        .show(|ui| {
                            ui.data_set_tab::<PopupOpened>(tab_id, PopupOpened(id, row.index()));
                            let options =
                                build_for_path(&tab.current_path, val.get_path(), &favorites);
                            for option in options {
                                if ui.button(option.name.as_str()).clicked() {
                                    option.action.clone().schedule();
                                    ui.close();
                                }
                            }
                            if !is_dir {
                                if ui.button("Open").clicked() {
                                    ActionToPerform::SystemOpen(val.path.clone()).schedule();
                                    ui.close();
                                    return;
                                }
                                #[cfg(windows)]
                                if ui.button("Show in explorer").clicked() {
                                    crate::windows_tools::display_in_explorer(val.get_path())
                                        .unwrap_or_else(|_| {
                                            toast!(Error, "Could not open in explorer");
                                        });
                                    ui.close();
                                }
                                let val_dir = PathBuf::from(val.get_splitted_path().0);
                                let matching_dirs =
                                    self.tab_paths.iter().filter(|s| !val_dir.eq(*s));
                                #[allow(clippy::collapsible_else_if)]
                                if matching_dirs.clone().count() > 0 {
                                    ui.separator();
                                    ui.menu_button("Move to", |ui| {
                                        for other in matching_dirs {
                                            let other = PathBuf::from(
                                                std::fs::canonicalize(other)
                                                    .unwrap_or_else(|_| {
                                                        val.get_path().to_path_buf()
                                                    })
                                                    .to_fixed_string(),
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

                                                let filename =
                                                    path.file_name().expect("NO FILENAME");
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
                                                                success =
                                                                    fs::remove_file(path).is_ok();
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
                                                ui.close();
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
                                ui.close();
                                TabAction::RequestFilesRefresh.schedule_active_tab();
                            }
                            if ui.button("Copy path to clipboard").clicked() {
                                let Ok(mut clipboard) = arboard::Clipboard::new() else {
                                    toast!(Error, "Failed to read the clipboard.");
                                    return;
                                };
                                clipboard.set_text(val.path.clone()).unwrap_or_else(|_| {
                                    toast!(Error, "Failed to update the clipboard.");
                                });
                                ui.close();
                            }
                            if ui.button("Rename").clicked() {
                                ui.data_mut(|w| {
                                    w.insert_temp(egui::Id::new(ModalWindow::Rename), val.clone());
                                });
                                ActionToPerform::ToggleModalWindow(ModalWindow::Rename).schedule();
                                ui.close();
                            }

                            #[cfg(windows)]
                            {
                                ui.separator();
                                if ui.button("Properties").clicked() {
                                    crate::windows_tools::open_properties(val.get_path());
                                    ui.close();
                                }
                            }
                        });
                });
            });
        if let Some(new_sort) = new_sort {
            let mut settings: DirectoryViewSettings =
                ui.data_get_path_or_persisted(&tab.current_path).data;
            settings.change_sort(new_sort);
            ui.data_set_path(&tab.current_path, settings);

            ActionToPerform::ViewSettingsChanged(crate::app::DataSource::Local).schedule();
        }
        let input_key = ui.input(|i| {
            if i.key_pressed(egui::Key::ArrowDown) {
                Some(egui::Key::ArrowDown)
            } else if i.key_pressed(egui::Key::ArrowUp) {
                Some(egui::Key::ArrowUp)
            } else if i.key_pressed(egui::Key::ArrowLeft) {
                Some(egui::Key::ArrowLeft)
            } else if i.key_pressed(egui::Key::Enter) {
                Some(egui::Key::Enter)
            } else if i.key_pressed(egui::Key::ArrowRight) {
                Some(egui::Key::ArrowRight)
            } else {
                None
            }
        });
        if !self.active_tab.is_some_and(|a| a.eq(&tab.id)) {
            return;
        }
        if let Some(change) = input_key {
            let new_value = match selected_tabs.0.first() {
                Some(i) => match change {
                    egui::Key::ArrowDown => {
                        i.saturating_add(1).min(tab.list.len().saturating_sub(1))
                    }
                    egui::Key::ArrowUp => i.saturating_sub(1),
                    egui::Key::ArrowLeft => {
                        if let Some(parent) = tab.current_path.parent() {
                            TabAction::ChangePaths(parent.into()).schedule_tab(tab.id);
                        }
                        return;
                    }
                    egui::Key::Enter | egui::Key::ArrowRight => {
                        let Some(entry) = tab.list.get(*i) else {
                            return;
                        };
                        if entry.is_file() {
                            ActionToPerform::SystemOpen(entry.path.clone()).schedule();
                        } else {
                            TabAction::ChangePaths(entry.get_path().to_path_buf().into())
                                .schedule_tab(tab_id);
                            return;
                        }
                        *i
                    }
                    _ => return,
                },
                None => 0,
            };
            selected_tabs.0.clear();
            selected_tabs.0.push(new_value);
            ui.data_set_path(&tab.current_path, selected_tabs.clone());
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
        let active_tab = self.get_current_tab()?;
        active_tab.current_path.single_path()
    }
    pub fn new(path: &Path) -> Self {
        // Create a `DockState` with an initial tab "tab1" in the main `Surface`'s root node.
        let tabs = vec![TabData::from_path(path)];
        let dock_state = DockState::new(tabs);
        Self { dock_state }
    }

    pub fn get_current_index(&mut self) -> Option<u32> {
        self.get_current_tab().map(|tab| tab.id)
    }

    pub fn get_tab_by_id(&mut self, id: u32) -> Option<&mut TabData> {
        self.dock_state
            .iter_all_tabs_mut()
            .find(|(_, tab)| tab.id == id)
            .map(|(_, tab)| tab)
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

    pub fn ui(&mut self, ui: &mut Ui) {
        let tabs = self.get_tabs_paths();
        let tabs_len = self.dock_state.iter_all_tabs().count();
        let active_tab = self.get_current_index();

        for ((_, _), item) in self.dock_state.iter_all_tabs_mut() {
            if item.check_for_file_system_events() {
                TabAction::RequestFilesRefresh.schedule_tab(item.id);
            }
        }
        let mut my_tab_viewer = MyTabViewer {
            closeable: tabs_len > 1,
            tab_paths: tabs,
            active_tab,
        };
        DockArea::new(&mut self.dock_state)
            .show_leaf_close_all_buttons(false)
            .show_leaf_collapse_buttons(false)
            .show_add_popup(true)
            .show_add_buttons(true)
            .style(Self::get_dock_style(ui.style().as_ref(), tabs_len))
            .show_inside(ui, &mut my_tab_viewer);
    }

    pub fn open_in_new_tab(&mut self, path: &Path) {
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
