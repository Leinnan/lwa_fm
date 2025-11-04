use egui::ahash::HashMap;
use egui::text::{LayoutJob, TextWrapping};
use egui::util::undoer::Undoer;
use egui::{
    Color32, Context, FontId, FontSelection, Galley, Id, Layout, Sense, TextBuffer, TextFormat, Ui,
    WidgetText,
};
use egui_dock::{DockArea, DockState, NodeIndex, Style, SurfaceIndex, TabViewer};
use icu::collator::options::{AlternateHandling, CollatorOptions, Strength};
use icu::collator::{Collator, CollatorBorrowed};
use icu::locale::Locale;
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::Path;
use std::sync::Arc;
use std::{ffi::OsStr, path::PathBuf};

use egui::Vec2;
use egui_extras::{Column, TableBuilder};

use crate::app::command_palette::build_for_path;
use crate::app::commands::{ModalWindow, TabAction, TabTarget};
use crate::data::time::ElapsedTime;
use crate::widgets::time_label::draw_size;
// use crate::app::dir_handling::COLLATER;
use super::commands::ActionToPerform;
use crate::app::directory_view_settings::{DirectoryShowHidden, DirectoryViewSettings};
use crate::app::top_bottom::TopDisplayPath;
use crate::app::{Search, Sort};
use crate::data::files::DirEntry;
use crate::helper::{DataHolder, KeyWithCommandPressed, PathFixer};
use crate::locations::Locations;
use crate::toast;
use crate::watcher::DirectoryWatcher;
use std::cell::RefCell;

thread_local! {
    pub static TIME_POOL: RefCell<HashMap<ElapsedTime, Arc<Galley>>> = RefCell::new(HashMap::default());
}

pub fn populate_time_pool(components: impl Iterator<Item = ElapsedTime>, ui: &Context) {
    TIME_POOL.with_borrow_mut(|pool| {
        for component in components {
            if !pool.contains_key(&component) {
                let galley = WidgetText::LayoutJob(Arc::new(LayoutJob::simple_singleline(
                    component.to_string(),
                    FontId::default(),
                    Color32::DARK_GRAY,
                )))
                .into_galley_impl(
                    ui,
                    &ui.style(),
                    TextWrapping::default(),
                    FontSelection::Default,
                    egui::Align::Center,
                );
                _ = pool.insert(component, galley);
            }
        }
    })
}

thread_local! {
    pub static SIZES_POOL: RefCell<HashMap<u64, Arc<Galley>>> = RefCell::new(HashMap::default());
}
pub fn populate_sizes_pool(components: impl Iterator<Item = u64>, ui: &Context) {
    SIZES_POOL.with_borrow_mut(|pool| {
        for component in components {
            if !pool.contains_key(&component) {
                let galley = WidgetText::LayoutJob(Arc::new(LayoutJob::simple_singleline(
                    crate::helper::format_bytes_simple(component),
                    FontId::default(),
                    Color32::DARK_GRAY,
                )))
                .into_galley_impl(
                    ui,
                    &ui.style(),
                    TextWrapping::default(),
                    FontSelection::Default,
                    egui::Align::Center,
                );
                _ = pool.insert(component, galley);
            }
        }
    })
}

#[derive(Debug, PartialEq, Eq, PartialOrd, Ord, Clone, Default, Hash)]
pub enum CurrentPath {
    #[default]
    None,
    One(PathBuf),
    Multiple(Vec<PathBuf>),
}

impl CurrentPath {
    pub fn get_path(&self) -> Option<PathBuf> {
        match self {
            Self::None => None,
            Self::One(path) => Some(path.clone()),
            Self::Multiple(paths) => {
                let mut common_path = paths.first()?.clone();
                for path in paths.iter().skip(1) {
                    let mut common_components = Vec::new();
                    let common_iter = common_path.components().zip(path.components());

                    for (a, b) in common_iter {
                        if a == b {
                            common_components.push(a);
                        } else {
                            break;
                        }
                    }

                    common_path = common_components.into_iter().collect();
                }
                Some(common_path)
            }
        }
    }
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
    pub visible_entries: Vec<usize>,
    pub current_path: CurrentPath,
    pub show_hidden: bool,
    pub search: Option<Search>,
    pub watcher: Option<DirectoryWatcher>,
    undoer: Undoer<CurrentPath>,
    pub id: u32,
    pub top_display_path: TopDisplayPath,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
struct PopupOpened(pub Id, pub usize);

impl Default for PopupOpened {
    fn default() -> Self {
        Self(Id::new(0), usize::MAX)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash, Default)]
pub struct Selected {
    pub selected_fields: Vec<usize>,
    pub just_changed: bool,
}

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
        let mut top_display_path = TopDisplayPath::default();
        top_display_path.build(path, false);
        let new = Self {
            id: get_id(),
            list: vec![],
            visible_entries: vec![],
            current_path: CurrentPath::None,
            show_hidden: false,
            search: None,
            watcher: DirectoryWatcher::new().ok(),
            undoer: Undoer::default(),
            top_display_path,
        };
        TabAction::ChangePaths(CurrentPath::One(path.into())).schedule_tab(new.id);
        new
    }
}

pub struct MyTabViewer {
    closeable: bool,
    tab_paths: Vec<PathBuf>,
    active_tab: u32,
    focused: bool,
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
        #[cfg(feature = "profiling")]
        puffin::profile_scope!("MyTabViewer::ui");
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
        if self.focused && ui.key_with_command_pressed(egui::Key::ArrowUp) && !tab.is_searching() {
            let Some(parent) = tab.current_path.parent() else {
                return;
            };
            TabAction::ChangePaths(parent.into()).schedule_tab(tab.id);
            return;
        }
        let shift_pressed = ui.input(|i| i.shift_pressed());
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
        let just_changed = if selected_tabs.just_changed {
            selected_tabs.just_changed = false;
            ui.data_set_path(&tab.current_path, selected_tabs.clone());
            true
        } else {
            false
        };
        let table_id = Id::new(tab.id).with(&tab.current_path);
        let mut table = TableBuilder::new(ui)
            .striped(true)
            .vscroll(true)
            .id_salt(table_id)
            .cell_layout(egui::Layout::left_to_right(egui::Align::Center))
            .column(Column::remainder().at_least(260.0))
            .column(Column::auto().at_most(150.0).at_least(100.0))
            .column(Column::auto().at_most(150.0).at_least(100.0))
            .sense(egui::Sense::click());
        let length = tab.visible_entries.len();
        if just_changed {
            if let Some(v) = selected_tabs.selected_fields.last()
                && *v < tab.list.len()
            {
                eprintln!("{:?}", selected_tabs.just_changed);
                table = table.scroll_to_row(*v, None);
            }
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
                #[cfg(feature = "profiling")]
                puffin::profile_scope!("MyTabViewer::ui::table_body");
                body.rows(text_height, length, |mut row| {
                    let index = tab.visible_entries[row.index()];
                    let val = &tab.list[index];
                    let is_dir = !val.is_file();
                    let indexed = row.index() + 1;

                    if row.index() == opened_popup {
                        row.set_hovered(true);
                    } else if selected_tabs.selected_fields.contains(&row.index()) {
                        row.set_selected(true);
                    }
                    row.col(|ui| {
                        #[cfg(feature = "profiling")]
                        puffin::profile_scope!("MyTabViewer::ui::table_body::first_column");
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

                        added_button.on_hover_ui(|ui| {
                            let ext = val
                                .get_path()
                                .extension()
                                .unwrap_or_default()
                                .to_ascii_lowercase();
                            if ext.eq(&OsStr::new("png"))
                                || ext.eq(&OsStr::new("jpg"))
                                || ext.eq(&OsStr::new("jpeg"))
                            {
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
                            } else {
                                ui.add(egui::Label::new(val.path.as_str()));
                            }
                        });
                    });

                    if row.response().double_clicked()
                        || convert_nr_to_egui_key(indexed)
                            .is_some_and(|k| row.response().ctx.key_with_command_pressed(k))
                    {
                        if val.is_file() {
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
                        if !shift_pressed {
                            selected_tabs.selected_fields.clear();
                        }
                        selected_tabs.selected_fields.push(row.index());
                        selected_tabs.just_changed = true;
                        row.response()
                            .ctx
                            .data_set_path(&tab.current_path, selected_tabs.clone());
                    }
                    row.col(|ui| {
                        #[cfg(feature = "profiling")]
                        puffin::profile_scope!("MyTabViewer::ui::table_body::time_column");
                        // let time = {
                        //     puffin::profile_scope!(
                        //         "MyTabViewer::ui::table_body::time_column::string_build"
                        //     );
                        //     let Some(time) = TIME_POOL
                        //         .with_borrow(|pool| pool.get(&val.since_modified).cloned())
                        //     else {
                        //         return;
                        //     };
                        //     time
                        // };
                        ui.with_layout(Layout::right_to_left(egui::Align::Center), |ui| {
                            ui.add(val.meta.since_modified);
                        });
                    });
                    row.col(|ui| {
                        #[cfg(feature = "profiling")]
                        puffin::profile_scope!("MyTabViewer::ui::table_body::size_column");
                        if is_dir {
                            ui.add_space(1.0);
                            return;
                        }

                        ui.with_layout(Layout::right_to_left(egui::Align::Center), |ui| {
                            draw_size(ui, val.meta.size)
                        });
                        // let Some(size) =
                        //     SIZES_POOL.with_borrow(|pool| pool.get(&val.size).cloned())
                        // else {
                        //     return;
                        // };
                        // ui.with_layout(Layout::right_to_left(egui::Align::Center), |ui| {
                        //     ui.add(
                        //         egui::Label::new(size)
                        //             .wrap_mode(egui::TextWrapMode::Truncate)
                        //             .selectable(false)
                        //             .sense(Sense::empty()),
                        //     );
                        // });
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
        if !self.focused || !self.active_tab.eq(&tab.id) {
            return;
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
        if let Some(change) = input_key {
            let new_value = match selected_tabs.selected_fields.last() {
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
            if !shift_pressed {
                selected_tabs.selected_fields.clear();
            }
            selected_tabs.selected_fields.push(new_value);
            selected_tabs.just_changed = true;
            ui.data_set_path(&tab.current_path, selected_tabs.clone());
        }
    }
}

// Here is a simple example of how you can manage a `DockState` of your application.
#[derive(Debug)]
pub struct MyTabs {
    dock_state: DockState<TabData>,
    pub focused: bool,
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
        Self {
            dock_state,
            focused: false,
        }
    }

    pub fn get_current_index(&mut self) -> Option<u32> {
        self.get_current_tab().map(|tab| tab.id)
    }

    #[inline]
    pub fn try_get_tab_by_target(&mut self, target: TabTarget) -> Option<&mut TabData> {
        let tab_id = match target {
            TabTarget::ActiveTab => self.get_current_index(),
            TabTarget::TabWithId(id) => Some(id),
        }?;
        self.get_tab_by_id(tab_id)
    }

    #[inline]
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
        #[cfg(feature = "profiling")]
        puffin::profile_scope!("MyTabs::ui");
        let tabs = self.get_tabs_paths();
        let tabs_len = self.dock_state.iter_all_tabs().count();
        let Some(active_tab) = self.get_current_index() else {
            return;
        };
        {
            #[cfg(feature = "profiling")]
            puffin::profile_scope!("MyTabs::ui::check_for_file_system_events");
            for ((_, _), item) in self.dock_state.iter_all_tabs_mut() {
                if item.check_for_file_system_events() {
                    TabAction::RequestFilesRefresh.schedule_tab(item.id);
                }
            }
        }
        let mut my_tab_viewer = MyTabViewer {
            closeable: tabs_len > 1,
            tab_paths: tabs,
            active_tab,
            focused: self.focused,
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
