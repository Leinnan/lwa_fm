use egui::ahash::HashMap;
use egui::{Ui, WidgetText};
use egui_dock::{DockArea, DockState, NodeIndex, Style, SurfaceIndex, TabViewer};
use std::fs;
use std::{cell::RefCell, ffi::OsStr, path::PathBuf, rc::Rc};

use egui::{RichText, Vec2};
use egui_extras::{Column, TableBuilder};

use crate::{
    consts::VERTICAL_SPACING,
    locations::{Location, Locations},
    toast,
};

use super::directory_view_settings::DirectoryViewSettings;
use super::NewPathRequest;

#[derive(Debug)]
pub struct TabData {
    pub name: String,
    pub list: Vec<walkdir::DirEntry>,
    pub path_change: Option<NewPathRequest>,
    pub current_path: PathBuf,
    pub settings: DirectoryViewSettings,
    pub locations: Rc<RefCell<HashMap<String, Locations>>>,
    pub other_tabs_paths: Vec<PathBuf>,
    pub dir_has_cargo: bool,
    pub can_close: bool,
}

impl TabData {
    pub fn from_path(path: &PathBuf, locations: Rc<RefCell<HashMap<String, Locations>>>) -> Self {
        let mut new = Self {
            list: vec![],
            path_change: None,
            name: String::new(),
            current_path: PathBuf::new(),
            settings: DirectoryViewSettings::default(),
            locations,
            dir_has_cargo: false,
            can_close: true,
            other_tabs_paths: vec![],
        };
        new.set_path(path);
        new
    }
    pub fn set_other_paths(&mut self, other_tabs: &[PathBuf]) {
        self.other_tabs_paths = other_tabs
            .iter()
            .filter(|p| !p.eq(&&self.current_path))
            .cloned()
            .collect::<Vec<PathBuf>>();
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
    fn title(&mut self, tab: &mut Self::Tab) -> WidgetText {
        if tab.settings.is_searching() {
            format!("Searching: {}", &tab.settings.search.value).into()
        } else {
            tab.name.as_str().into()
        }
    }

    /// Defines the contents of a given `tab`.
    #[allow(clippy::too_many_lines)]
    fn ui(&mut self, ui: &mut Ui, tab: &mut Self::Tab) {
        tab.path_change = None;
        let mut require_refresh = false; //  replace it with flow using https://docs.rs/notify/latest/notify/
        egui::ScrollArea::vertical().show(ui, |ui| {
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
                    let Ok(meta) = val.metadata() else {
                        return;
                    };
                    row.col(|ui| {
                        ui.add_space(VERTICAL_SPACING);
                        // ui.allocate_space(egui::vec2(available_width, 20.0));
                        let file_type = meta.file_type();
                        #[cfg(target_os = "windows")]
                        let is_dir = {
                            use std::os::windows::fs::FileTypeExt;
                            file_type.is_dir() || file_type.is_symlink_dir()
                        };
                        #[cfg(not(target_os = "windows"))]
                        let is_dir = file_type.is_dir();
                        let text = val.file_name().to_string_lossy();

                        let text = if is_dir {
                            RichText::new(text)
                        } else {
                            RichText::strong(text.into())
                        };
                        let added_button = ui
                            .add(egui::Button::new(text).fill(egui::Color32::from_white_alpha(0)));

                        if added_button.clicked() {
                            if meta.is_file() {
                                let _ = open::that_detached(val.path());
                            } else {
                                let Ok(path) = std::fs::canonicalize(val.path()) else {
                                    return;
                                };
                                tab.path_change = Some(NewPathRequest {
                                    new_tab: ui.input(|i| i.modifiers.ctrl),
                                    path,
                                });
                            }
                        }
                        added_button.context_menu(|ui| {
                            if is_dir {
                                if ui.button("Open in new tab").clicked() {
                                    tab.path_change = Some(NewPathRequest {
                                        new_tab: true,
                                        path: val.path().to_path_buf(),
                                    });
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
                                    open::that_detached(val.path())
                                        .expect("Failed to open directory in file manager");
                                    ui.close_menu();
                                    return;
                                }
                                let Ok(path) = std::fs::canonicalize(val.path()) else {
                                    return;
                                };
                                let mut locations = tab.locations.borrow_mut();

                                let existing_path =
                                    locations.get("Favorites").and_then(|favorites| {
                                        favorites
                                            .locations
                                            .iter()
                                            .enumerate()
                                            .find(|(_i, loc)| path.ends_with(&loc.path))
                                            .map(|(i, _)| i)
                                    });

                                if existing_path.is_none()
                                    && ui.button("Add to favorites").clicked()
                                {
                                    if let Some(name) = path.iter().last() {
                                        if let Some(fav) = locations.get_mut("Favorites") {
                                            fav.locations.push(Location {
                                                name: name.to_string_lossy().to_string(),
                                                path,
                                            });
                                        } else {
                                            locations.insert(
                                                "Favorites".into(),
                                                Locations {
                                                    editable: true,
                                                    ..Default::default()
                                                },
                                            );
                                            if let Some(fav) = locations.get_mut("Favorites") {
                                                fav.locations.push(Location {
                                                    name: name.to_string_lossy().to_string(),
                                                    path,
                                                });
                                            } else {
                                                toast!(Error, "Failed to add favorite");
                                            }
                                        }
                                    } else {
                                        toast!(Error, "Could not get name of file");
                                    }

                                    ui.close_menu();
                                    return;
                                }

                                if existing_path.is_some()
                                    && ui.button("Remove from favorites").clicked()
                                {
                                    if let Some(fav) = locations.get_mut("Favorites") {
                                        if let Some(existing_path) = existing_path {
                                            fav.locations.remove(existing_path);
                                        }
                                    }
                                    ui.close_menu();
                                }
                            } else {
                                #[cfg(windows)]
                                if ui.button("Show in explorer").clicked() {
                                    crate::windows_tools::display_in_explorer(val.path())
                                        .unwrap_or_else(|_| {
                                            toast!(Error, "Could not open in explorer");
                                        });
                                    ui.close_menu();
                                }
                                if !tab.other_tabs_paths.is_empty() {
                                    ui.separator();
                                    ui.menu_button("Move to", |ui| {
                                        for other in &tab.other_tabs_paths {
                                            let other = PathBuf::from(
                                                std::fs::canonicalize(other)
                                                    .unwrap_or_else(|_| val.path().to_path_buf())
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
                                                let filename =
                                                    val.path().file_name().expect("NO FILENAME");
                                                let target_path = other.join(filename);
                                                println!("{}", &target_path.display());
                                                fs::rename(val.path(), target_path).unwrap_or_else(
                                                    |e| {
                                                        toast!(
                                                            Error,
                                                            "Failed to move file {}: {}",
                                                            filename.to_string_lossy(),
                                                            e
                                                        );
                                                    },
                                                );
                                                require_refresh = true;
                                                ui.close_menu();
                                            }
                                        }
                                    });
                                }
                            }
                            ui.separator();
                            if ui.button("Move to Trash").clicked() {
                                trash::delete(val.path()).unwrap_or_else(|_| {
                                    toast!(Error, "Could not move it to trash.");
                                });
                                ui.close_menu();
                            }
                            ui.separator();
                            #[cfg(windows)]
                            if ui.button("Properties").clicked() {
                                crate::windows_tools::open_properties(val.path());
                                ui.close_menu();
                            }
                        });
                        let ext = val
                            .path()
                            .extension()
                            .unwrap_or_default()
                            .to_ascii_lowercase();
                        if ext.eq(&OsStr::new("png"))
                            || ext.eq(&OsStr::new("jpg"))
                            || ext.eq(&OsStr::new("jpeg"))
                        {
                            added_button.on_hover_ui(|ui| {
                                let path = std::fs::canonicalize(val.path())
                                    .unwrap_or_else(|_| val.path().to_path_buf())
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
                                std::fs::canonicalize(val.path())
                                    .unwrap_or_else(|_| val.path().to_path_buf())
                            ));
                        }
                    });
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
    pub fn get_current_path(&mut self) -> PathBuf {
        let Some(active_tab) = self.get_current_tab() else {
            return PathBuf::new();
        };
        active_tab.current_path.clone()
    }
    pub fn new(path: &PathBuf, locations: Rc<RefCell<HashMap<String, Locations>>>) -> Self {
        // Create a `DockState` with an initial tab "tab1" in the main `Surface`'s root node.
        let tabs = vec![TabData::from_path(path, locations)];
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

    pub fn update_active_tab(&mut self, path: &PathBuf) {
        let Some(active_tab) = self.get_current_tab() else {
            return;
        };
        active_tab.set_path(path);
    }

    pub fn ui(&mut self, ui: &mut Ui) {
        let tabs_amount = self.dock_state.iter_all_tabs().count();
        let tabs: Vec<PathBuf> = self
            .dock_state
            .iter_all_tabs()
            .map(|(_, tab)| tab.current_path.clone())
            .collect();

        for ((_, _), item) in self.dock_state.iter_all_tabs_mut() {
            item.can_close = tabs_amount > 1;
            item.set_other_paths(&tabs);
        }
        let mut style = Style::from_egui(ui.style().as_ref());
        style.dock_area_padding = None;
        style.tab_bar.fill_tab_bar = true;
        style.tab_bar.height = if tabs_amount > 1 {
            style.tab_bar.height * 1.4
        } else {
            0.0
        };
        style.tab.tab_body.inner_margin = egui::Margin::same(10.0);
        style.tab.tab_body.stroke = egui::Stroke::NONE;
        DockArea::new(&mut self.dock_state)
            .style(style)
            .show_inside(ui, &mut MyTabViewer);

        let Some(active_tab) = self.get_current_tab() else {
            return;
        };
        if let Some(path_change) = &active_tab.path_change {
            if path_change.new_tab {
                let new_window =
                    TabData::from_path(&path_change.path, Rc::clone(&active_tab.locations));

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
            } else {
                active_tab.set_path(&path_change.path.clone());
            }
        }
    }

    pub fn open_in_new_tab(&mut self, path: &PathBuf) {
        let is_not_focused = self.dock_state.focused_leaf().is_none();
        if is_not_focused {
            self.dock_state
                .set_focused_node_and_surface((SurfaceIndex::main(), NodeIndex::root()));
        }
        let Some(active_tab) = self.get_current_tab() else {
            return;
        };
        let new_window = TabData::from_path(path, Rc::clone(&active_tab.locations));
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
}
