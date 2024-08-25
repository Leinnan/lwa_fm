use egui::{Ui, WidgetText};
use egui_dock::{DockArea, DockState, Style, TabViewer};
use std::{ffi::OsStr, path::PathBuf};

use egui::{RichText, Vec2};
use egui_extras::{Column, TableBuilder};

use crate::{
    consts::VERTICAL_SPACING,
    toast,
};
// First, let's pick a type that we'll use to attach some data to each tab.
// It can be any type.
pub struct TabData {
    pub name: String,
    list: Vec<walkdir::DirEntry>,
    new_path: Option<PathBuf>,
}

impl TabData {
    pub fn from_path(path: PathBuf) -> Self {
        Self {
            list: Self::read_dir(&path),
            new_path: None,
            name: path.clone().as_os_str().to_string_lossy().to_string(),
        }
    }
    fn read_dir(path: &PathBuf) -> Vec<walkdir::DirEntry> {
        let directories = [&path].to_vec();

        // let depth = if use_search { self.search.depth } else { 1 };
        let dir_entries: Vec<walkdir::DirEntry> = directories
            .iter()
            .flat_map(|d| {
                walkdir::WalkDir::new(d)
                    .follow_links(true)
                    .max_depth(1)
                    .into_iter()
                    .flatten()
                    .skip(1)
                    // .filter(|e| {
                    //     let s = e.file_name().to_string_lossy();
                    //     if !self.show_hidden && (s.starts_with('.') || s.starts_with('$')) {
                    //         return false;
                    //     }
                    //     if self.search.case_sensitive {
                    //         s.contains(search)
                    //     } else {
                    //         s.to_ascii_lowercase()
                    //             .contains(&search.to_ascii_lowercase())
                    //     }
                    // })
                    .collect::<Vec<walkdir::DirEntry>>()
            })
            .collect();
        // if self.sorting == Sort::Random {
        //     use rand::seq::SliceRandom;
        //     use rand::thread_rng;
        //     let mut rng = thread_rng();

        //     dir_entries.shuffle(&mut rng);
        //     return dir_entries;
        // }
        // dir_entries.sort_by(|a, b| {
        //     a.file_type()
        //         .is_file()
        //         .cmp(&b.file_type().is_file())
        //         .then(match &self.sorting {
        //             Sort::Random => panic!(),
        //             Sort::Name => a
        //                 .file_name()
        //                 .to_ascii_lowercase()
        //                 .cmp(&b.file_name().to_ascii_lowercase()),
        //             Sort::Modified => a
        //                 .metadata()
        //                 .unwrap()
        //                 .modified()
        //                 .unwrap()
        //                 .cmp(&b.metadata().unwrap().modified().unwrap()),
        //             Sort::Created => a
        //                 .metadata()
        //                 .unwrap()
        //                 .created()
        //                 .unwrap()
        //                 .cmp(&b.metadata().unwrap().created().unwrap()),
        //             #[cfg(windows)]
        //             Sort::Size => {
        //                 std::os::windows::fs::MetadataExt::file_size(&a.metadata().unwrap()).cmp(
        //                     &std::os::windows::fs::MetadataExt::file_size(&b.metadata().unwrap()),
        //                 )
        //             }
        //             #[cfg(target_os = "linux")]
        //             Sort::Size => std::os::linux::fs::MetadataExt::st_size(&a.metadata().unwrap())
        //                 .cmp(&std::os::linux::fs::MetadataExt::st_size(
        //                     &b.metadata().unwrap(),
        //                 )),
        //             #[cfg(target_os = "macos")]
        //             Sort::Size => std::os::macos::fs::MetadataExt::st_size(&a.metadata().unwrap())
        //                 .cmp(&std::os::macos::fs::MetadataExt::st_size(
        //                     &b.metadata().unwrap(),
        //                 )),
        //         })
        // });
        dir_entries
    }
}

// To define the contents and properties of individual tabs, we implement the `TabViewer`
// trait. Only three things are mandatory: the `Tab` associated type, and the `ui` and
// `title` methods. There are more methods in `TabViewer` which you can also override.
pub struct MyTabViewer;

impl TabViewer for MyTabViewer {
    // This associated type is used to attach some data to each tab.
    type Tab = TabData;

    // Returns the current `tab`'s title.
    fn title(&mut self, tab: &mut Self::Tab) -> WidgetText {
        tab.name.as_str().into()
    }

    // Defines the contents of a given `tab`.
    fn ui(&mut self, ui: &mut Ui, tab: &mut Self::Tab) {
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
                                tab.new_path = Some(path);
                            }
                        }
                        added_button.context_menu(|ui| {
                            if is_dir {
                                #[cfg(windows)]
                                if ui.button("Open in explorer").clicked() {
                                    crate::windows_tools::open_in_explorer(val.path(), true)
                                        .unwrap_or_else(|_| {
                                            toast!(Error, "Could not open in explorer");
                                        });
                                    ui.close_menu();
                                    return;
                                }
                                #[cfg(target_os = "macos")]
                                if ui.button("Open in Finder").clicked() {
                                    open::that_detached(val.path())
                                        .expect("Failed to open dir in Finder");
                                    ui.close_menu();
                                    return;
                                }
                                let Ok(path) = std::fs::canonicalize(val.path()) else {
                                    return;
                                };

                                // let existing_path =
                                //     self.locations.get("Favorites").and_then(|favorites| {
                                //         favorites
                                //             .locations
                                //             .iter()
                                //             .enumerate()
                                //             .find(|(_i, loc)| path.ends_with(&loc.path))
                                //             .map(|(i, _)| i)
                                //     });

                                // if existing_path.is_none()
                                //     && ui.button("Add to favorites").clicked()
                                // {
                                //     if let Some(name) = path.iter().last() {
                                //         if let Some(fav) = self.locations.get_mut("Favorites") {
                                //             fav.locations.push(Location {
                                //                 name: name.to_string_lossy().to_string(),
                                //                 path,
                                //             });
                                //         }
                                //     } else {
                                //         toast!(Error, "Could not get name of file");
                                //     }

                                //     ui.close_menu();
                                //     return;
                                // }

                                // if existing_path.is_some()
                                //     && ui.button("Remove from favorites").clicked()
                                // {
                                //     if let Some(fav) = self.locations.get_mut("Favorites") {
                                //         if let Some(existing_path) = existing_path {
                                //             fav.locations.remove(existing_path);
                                //         }
                                //     }
                                //     ui.close_menu();
                                // }
                            } else {
                                #[cfg(windows)]
                                if ui.button("Show in explorer").clicked() {
                                    crate::windows_tools::open_in_explorer(val.path(), false)
                                        .unwrap_or_else(|_| {
                                            toast!(Error, "Could not open in explorer");
                                        });
                                    ui.close_menu();
                                }
                            }
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
    }
}

// Here is a simple example of how you can manage a `DockState` of your application.
pub struct MyTabs {
    dock_state: DockState<TabData>,
}

impl MyTabs {
    pub fn new(path: PathBuf) -> Self {
        // Create a `DockState` with an initial tab "tab1" in the main `Surface`'s root node.
        let tabs = vec![TabData::from_path(path)];
        let dock_state = DockState::new(tabs);
        Self { dock_state }
    }

    pub fn ui(&mut self, ui: &mut Ui) {
        // Here we just display the `DockState` using a `DockArea`.
        // This is where egui handles rendering and all the integrations.
        //
        // We can specify a custom `Style` for the `DockArea`, or just inherit
        // all of it from egui.
        DockArea::new(&mut self.dock_state)
            .style(Style::from_egui(ui.style().as_ref()))
            .show_inside(ui, &mut MyTabViewer);
    }
}
