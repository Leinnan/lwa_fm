use egui::ahash::HashMap;
use egui::text::{LayoutJob, TextWrapping};
use egui::util::undoer::Undoer;
use egui::{
    Color32, Context, FontId, FontSelection, Galley, Id, Layout, Sense, TextBuffer, TextFormat, Ui,
    WidgetText,
};
use egui_dock::{DockArea, DockState, Style, TabViewer};
use icu::collator::options::{AlternateHandling, CollatorOptions, Strength};
use icu::collator::{Collator, CollatorBorrowed};
use icu::locale::Locale;
use mlua::{Function, UserData};
use serde::{Deserialize, Serialize};
use std::borrow::Cow;
use std::fs;
use std::path::Path;
use std::path::PathBuf;
use std::sync::Arc;

use egui::Vec2;
use egui_taffy::{
    TuiBuilderLogic, taffy, tid, tui,
    virtual_tui::{VirtualGridRowHelper, VirtualGridRowHelperParams},
};
use taffy::prelude::*;
use taffy::style_helpers;

use super::assets::{AssetManager, HoverPreview};
use super::commands::ActionToPerform;
use crate::app::command_palette::build_for_path;
use crate::app::commands::{ModalWindow, TabAction, TabTarget};
use crate::app::directory_view_settings::{DirectoryShowHidden, DirectoryViewSettings};
use crate::app::top_bottom::TopDisplayPath;
use crate::app::{DisplayType, LUA_INSTANCE, Search, Sort};
use crate::data::files::DirEntry;
use crate::data::time::ElapsedTime;
use crate::helper::{DataHolder, KeyWithCommandPressed, PathFixer, PathHelper};
use crate::locations::Locations;
use crate::toast;
use crate::widgets::time_label::draw_size;
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
    pub static SIZES_POOL: RefCell<HashMap<u32, Arc<Galley>>> = RefCell::new(HashMap::default());
}
pub fn populate_sizes_pool(components: impl Iterator<Item = u32>, ui: &Context) {
    SIZES_POOL.with_borrow_mut(|pool| {
        for component in components {
            if !pool.contains_key(&component) {
                let galley = WidgetText::LayoutJob(Arc::new(LayoutJob::simple_singleline(
                    crate::helper::format_bytes_simple(component as u64),
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

impl UserData for CurrentPath {
    fn add_fields<F: mlua::UserDataFields<Self>>(_: &mut F) {}

    fn add_methods<M: mlua::UserDataMethods<Self>>(methods: &mut M) {
        methods.add_method("print_path", |c, this, ()| {
            let path = this.get_name_from_path();
            let full_path: Cow<'static, str> = match this {
                CurrentPath::One(path_buf) => path_buf
                    .to_full_path()
                    .map(|p| Cow::Owned(p.to_string_lossy().to_string())),
                _ => None,
            }
            .unwrap_or_default();
            let print: Function = c.globals().get("print")?;
            print.call::<()>(("FROM LUA: ", path.as_str()))?;
            print.call::<()>(("Full path: ", full_path.as_ref()))?;
            Ok(())
        });

        // Constructor
        // methods.add_meta_function(MetaMethod::Call, |_, ()| Ok(Rectangle::default()));
    }
}

impl CurrentPath {
    pub fn print_from_lua(&self) {
        let data = self.clone();
        LUA_INSTANCE.with_borrow(|f| {
            let globals = f.globals();
            _ = globals.set("cur_dir", data);
            let _ = f.load("cur_dir:print_path()").exec();
        });
    }
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
    pub display_type: DisplayType,
    pub search: Option<Search>,
    // pub watcher: Option<DirectoryWatcher>,
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
            let was_deep = search.depth > 1;
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
            } else if was_deep {
                TabAction::RequestFilesRefresh.schedule_tab(self.id);
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
            display_type: DisplayType::default(),
            search: None,
            // watcher: DirectoryWatcher::new().ok(),
            undoer: Undoer::default(),
            top_display_path,
        };
        TabAction::ChangePaths(CurrentPath::One(path.into())).schedule_tab(new.id);
        new
    }
}

pub struct MyTabViewer<'a> {
    closeable: bool,
    tab_paths: Vec<PathBuf>,
    active_tab: u32,
    focused: bool,
    assets: &'a mut AssetManager,
}

// Column dimension constants used in both the header and data rows of the file grid.
const COL_MODIFIED_W: f32 = 125.0;
const COL_MODIFIED_MIN: f32 = 100.0;
const COL_MODIFIED_MAX: f32 = 150.0;
const COL_SIZE_W: f32 = 125.0;
const COL_SIZE_MIN: f32 = 100.0;
const COL_SIZE_MAX: f32 = 150.0;
const GRID_TILE_WIDTH: f32 = 168.0;
const GRID_TILE_HEIGHT: f32 = 148.0;
const GRID_TILE_PADDING: f32 = 10.0;
const GRID_TILE_PREVIEW_SIZE: f32 = 84.0;
const GRID_TILE_TEXT_HEIGHT: f32 = 20.0;

/// Captured row interaction result collected while the taffy tui borrow is active,
/// so they can be processed afterwards (when `tab` can be borrowed again).
struct RowResult {
    row_index: usize,
    response: egui::Response,
}

impl MyTabViewer<'_> {
    fn grid_view(&mut self, ui: &mut Ui, tab: &mut TabData) {
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

        let shift_pressed = ui.input(egui::InputState::shift_pressed);
        let favorites = ui.data_get_persisted::<Locations>().unwrap_or_default();
        let tab_id = tab.id;
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

        let entries_len = tab.visible_entries.len();
        let tab_popup_id = Id::new(tab_id).with(1500);
        let tile_width = GRID_TILE_WIDTH;
        let tile_height = GRID_TILE_HEIGHT;
        let item_spacing = ui.spacing().item_spacing;
        let columns = ((ui.available_width() + item_spacing.x) / (tile_width + item_spacing.x))
            .floor()
            .max(1.0) as usize;
        let full_row_spacing = if columns > 1 {
            let used_width = columns as f32 * tile_width + (columns - 1) as f32 * item_spacing.x;
            let leftover_width = (ui.available_width() - used_width).max(0.0);
            item_spacing.x + leftover_width / columns as f32
        } else {
            item_spacing.x
        };
        let row_count = entries_len.div_ceil(columns);

        let mut row_results: Vec<RowResult> = Vec::with_capacity(entries_len.min(64));

        egui::ScrollArea::vertical()
            .auto_shrink([false, false])
            .show_rows(ui, tile_height, row_count, |ui, row_range| {
                for row in row_range {
                    let row_start = row * columns;
                    let row_items = (entries_len - row_start).min(columns);
                    ui.horizontal_top(|ui| {
                        let row_spacing = if row_items > 1 {
                            full_row_spacing
                        } else {
                            item_spacing.x
                        };
                        ui.spacing_mut().item_spacing.x = row_spacing;

                        for col in 0..columns {
                            let row_index = row * columns + col;
                            if row_index >= entries_len {
                                break;
                            }

                            let index = tab.visible_entries[row_index];
                            let val = &tab.list[index];
                            let response = self.draw_grid_tile(
                                ui,
                                val,
                                tile_width,
                                tile_height,
                                selected_tabs.selected_fields.contains(&row_index),
                                row_index == opened_popup,
                                cmd,
                                row_index + 1,
                            );
                            row_results.push(RowResult {
                                row_index,
                                response,
                            });
                        }
                    });
                }
            });

        self.process_grid_interactions(
            ui,
            tab,
            &mut selected_tabs,
            shift_pressed,
            just_changed,
            tab_popup_id,
            &favorites,
            &row_results,
        );
        self.handle_grid_keyboard_navigation(ui, tab, &mut selected_tabs, shift_pressed, columns);
    }
    fn list_view(&mut self, ui: &mut Ui, tab: &mut TabData) {
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

        let multiple_dirs = tab.deep_or_multiple_paths();
        let text_height = (egui::TextStyle::Body.resolve(ui.style()).size * 1.5).ceil();

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
        let entries_len = tab.visible_entries.len();

        let mut new_sort = None;

        // ── Unified virtual-grid: sticky header row + scrollable body ────────
        //
        // The grid has 3 columns:  [Name (flex) | Modified (fixed) | Size (fixed)]
        // Row 1 is the sticky header.  Data rows follow via VirtualGridRowHelper.
        // Row interactions are collected into a Vec and processed after the tui
        // closure, because we cannot borrow `tab` inside the closure.
        let header_row_count: u16 = 1;

        // We need to collect row responses outside the tui closure because
        // borrowing `tab` inside it is not possible.
        let mut row_results: Vec<RowResult> = Vec::with_capacity(entries_len.min(64));

        tui(ui, Id::new("file_grid").with(tab.id))
        .reserve_available_space()
        .style(taffy::Style {
            padding: Rect {
                left: LengthPercentage::ZERO,
                right: LengthPercentage::ZERO,
                top: LengthPercentage::ZERO,
                bottom: LengthPercentage::ZERO,
            },
            margin: Rect {
                left: LengthPercentageAuto::ZERO,
                right: LengthPercentageAuto::ZERO,
                top: LengthPercentageAuto::ZERO,
                bottom: LengthPercentageAuto::ZERO,
            },
            flex_direction: taffy::FlexDirection::Column,
            size: percent(1.),
            max_size: percent(1.),
            ..Default::default()
        })
        .show(|tui| {
            // ── Scrollable grid container ────────────────────────────────
            tui.style(taffy::Style {
                display: taffy::Display::Grid,
                overflow: taffy::Point {
                    x: taffy::Overflow::Hidden,
                    y: taffy::Overflow::Scroll,
                },
                margin: Rect {
                    left: LengthPercentageAuto::ZERO,
                    right: LengthPercentageAuto::ZERO,
                    top: LengthPercentageAuto::ZERO,
                    bottom: LengthPercentageAuto::ZERO,
                },
                padding: Rect {
                    left: LengthPercentage::ZERO,
                    right: LengthPercentage::ZERO,
                    top: LengthPercentage::ZERO,
                    bottom: LengthPercentage::ZERO,
                },
                // 3 columns: Name (1fr, fills remaining), Modified (fixed), Size (fixed)
                grid_template_columns: vec![fr(1.), length(COL_MODIFIED_W), length(COL_SIZE_W)],
                size: taffy::Size {
                    width: percent(1.),
                    height: auto(),
                },
                max_size: percent(1.),
                grid_auto_rows: vec![min_content()],
                ..Default::default()
            })
            .add(|tui| {
                // ── Virtual data rows ────────────────────────────────────
                VirtualGridRowHelper::show(
                    VirtualGridRowHelperParams {
                        header_row_count,
                        row_count: entries_len,
                    },
                    tui,
                    |tui, info| {
                        #[cfg(feature = "profiling")]
                        puffin::profile_scope!("lwa_fm::MyTabViewer::ui::table_body");

                        let mut idgen = info.id_gen();
                        let grid_row_param = info.grid_row_setter();

                        let row_index = info.idx;
                        let index = tab.visible_entries[row_index];
                        let val = &tab.list[index];
                        let is_dir = !val.is_file();
                        let indexed = row_index + 1;

                        let is_hovered = row_index == opened_popup;
                        let is_selected = selected_tabs.selected_fields.contains(&row_index);

                        let bg_color = if is_selected {
                            tui.egui_ui().style().visuals.selection.bg_fill
                        } else if is_hovered {
                            tui.egui_ui().style().visuals.widgets.hovered.bg_fill
                        } else if row_index % 2 == 0 {
                            tui.egui_ui().style().visuals.faint_bg_color
                        } else {
                            egui::Color32::TRANSPARENT
                        };
                        let height = length(text_height.max(self.assets.render_size()) * self.assets.row_height_multiplier());
                        let min_size = taffy::Size {
                            width: length(0.0),
                            height,
                        };

                        // ── Name column ───────────────────────────────
                        let name_response = tui
                            .id(idgen())
                            .mut_style(&grid_row_param)
                            .mut_style(|style| {
                                style.min_size = min_size.clone();
                                style.overflow.x = taffy::Overflow::Hidden;
                                style.align_items = Some(taffy::AlignItems::Stretch);
                            })
                            .add_with_background_ui(
                                |ui, container| {
                                    ui.painter().rect_filled(
                                        container.full_container(),
                                        0.0,
                                        bg_color,
                                    );
                                },
                                |tui, ()| {
                                    tui.mut_style(|style| {
                                        style.padding =
                                            Rect {
                                                left: LengthPercentage::length(10.0),
                                                right: LengthPercentage::length(10.0),
                                                top: LengthPercentage::ZERO,
                                                bottom: LengthPercentage::ZERO,
                                            };
                                        style.size.width = percent(1.);
                                        style.overflow.x = taffy::Overflow::Hidden;
                                        style.align_items =
                                            Some(taffy::AlignItems::Center);
                                    })
                                    .ui(|ui: &mut Ui| {
                                        #[cfg(feature = "profiling")]
                                        puffin::profile_scope!(
                                            "lwa_fm::MyTabViewer::ui::table_body::first_column"
                                        );
                                        let color = if is_dir {
                                            Color32::LIGHT_GRAY
                                        } else {
                                            Color32::GRAY
                                        };

                                         ui.with_layout(
                                             Layout::left_to_right(egui::Align::Center),
                                             |ui| {
                                                  if let Some(texture) = self.assets.request_entry_texture(val) {
                                                      let render_sz = self.assets.render_size_for(val);
                                                      ui.add(
                                                          egui::Image::new(&texture)
                                                              .fit_to_exact_size(Vec2::splat(render_sz)),
                                                      );
                                                  } else {
                                                      ui.allocate_space(Vec2::splat(self.assets.render_size_for(val)));
                                                  }
                                                 if cmd && indexed < 10 {
                                                     ui.add(
                                                         egui::Label::new(
                                                            LayoutJob::simple_format(
                                                                format!("[{indexed}]"),
                                                                TextFormat {
                                                                    color: Color32::DARK_GRAY,
                                                                    ..Default::default()
                                                                },
                                                            ),
                                                        )
                                                        .wrap_mode(
                                                            egui::TextWrapMode::Truncate,
                                                        )
                                                        .selectable(false)
                                                        .sense(Sense::empty()),
                                                    );
                                                }
                                                let (dir, file) = val.get_splitted_path();
                                                if is_searching || multiple_dirs {
                                                    ui.add(
                                                        egui::Label::new(
                                                            LayoutJob::simple_singleline(
                                                                dir.into(),
                                                                FontId::default(),
                                                                Color32::DARK_GRAY,
                                                            ),
                                                        )
                                                        .wrap_mode(
                                                            egui::TextWrapMode::Truncate,
                                                        )
                                                        .selectable(false)
                                                        .sense(Sense::empty()),
                                                    );
                                                }

                                                let added_button = ui.add(
                                                    egui::Label::new(
                                                        LayoutJob::simple_singleline(
                                                            file.into(),
                                                            FontId::default(),
                                                            color,
                                                        ),
                                                    )
                                                    .wrap_mode(egui::TextWrapMode::Truncate)
                                                    .selectable(false)
                                                    .sense(Sense::empty()),
                                                );

                                                added_button.on_hover_ui(|ui| {
                                                    Self::show_entry_hover_preview(
                                                        self.assets,
                                                        ui,
                                                        val,
                                                    );
                                                });
                                            },
                                        )
                                        .response
                                    })
                                },
                            )
                            .main;

                        // ── Modified column ───────────────────────────
                        tui.id(idgen())
                            .mut_style(&grid_row_param)
                            .mut_style(|style| {
                                style.padding = Rect {
                                    left: LengthPercentage::length(10.0),
                                    right: LengthPercentage::length(10.0),
                                    top: LengthPercentage::ZERO,
                                    bottom: LengthPercentage::ZERO,
                                };
                                style.size = taffy::Size {
                                    width: length(COL_MODIFIED_W),
                                    height: auto(),
                                };
                                style.min_size = taffy::Size {
                                    width: length(COL_MODIFIED_MIN),
                                    height,
                                };
                                style.max_size = taffy::Size {
                                    width: length(COL_MODIFIED_MAX),
                                    height: auto(),
                                };
                                style.align_items = Some(taffy::AlignItems::Stretch);
                                // style.align_items = Some(taffy::AlignItems::Center);
                            })
                            .add_with_background_ui(
                                |ui, container| {
                                    ui.painter().rect_filled(
                                        container.full_container(),
                                        0.0,
                                        bg_color,
                                    );
                                },
                                |tui, ()| {
                                    tui.mut_style(|style| {
                                        style.size.width = percent(1.);
                                    })
                                    .ui(|ui: &mut Ui| {
                                        #[cfg(feature = "profiling")]
                                        puffin::profile_scope!(
                                            "lwa_fm::MyTabViewer::ui::table_body::time_column"
                                        );

                                        ui.with_layout(
                                            Layout::right_to_left(egui::Align::Center),
                                            |ui| {
                                                ui.add(val.meta.since_modified);
                                            },
                                        )
                                        .response
                                    })
                                },
                            );

                        // ── Size column ───────────────────────────────
                        tui.id(idgen())
                            .mut_style(&grid_row_param)
                            .mut_style(|style| {
                                style.padding = Rect {
                                    left: LengthPercentage::length(10.0),
                                    right: LengthPercentage::length(10.0),
                                    top: LengthPercentage::ZERO,
                                    bottom: LengthPercentage::ZERO,
                                };
                                style.size = taffy::Size {
                                    width: length(COL_SIZE_W),
                                    height: auto(),
                                };
                                style.min_size = taffy::Size {
                                    width: length(COL_SIZE_MIN),
                                    height,
                                };
                                style.max_size = taffy::Size {
                                    width: length(COL_SIZE_MAX),
                                    height: auto(),
                                };
                                style.align_items = Some(taffy::AlignItems::Stretch);
                                // style.align_items = Some(taffy::AlignItems::Center);
                            })
                            .add_with_background_ui(
                                |ui, container| {
                                    ui.painter().rect_filled(
                                        container.full_container(),
                                        0.0,
                                        bg_color,
                                    );
                                },
                                |tui, ()| {
                                    tui.mut_style(|style| {
                                        style.size.width = percent(1.);
                                    })
                                    .ui(|ui: &mut Ui| {
                                        #[cfg(feature = "profiling")]
                                        puffin::profile_scope!(
                                            "lwa_fm::MyTabViewer::ui::table_body::size_column"
                                        );

                                        if is_dir {
                                            ui.allocate_response(
                                                egui::Vec2::ZERO,
                                                Sense::empty(),
                                            )
                                        } else {
                                            ui.with_layout(
                                                Layout::right_to_left(egui::Align::Center),
                                                |ui| {
                                                    draw_size(ui, val.meta.size);
                                                },
                                            )
                                            .response
                                        }
                                    })
                                },
                            );

                        // ── Row interaction sense (full row) ──────────
                        // Build a rect that spans the full available width at the
                        // same vertical position as the name cell.
                        let available_width =
                            tui.egui_ui().available_rect_before_wrap().width();
                        let full_row_rect = egui::Rect::from_min_max(
                            egui::pos2(name_response.rect.left(), name_response.rect.top()),
                            egui::pos2(
                                name_response.rect.left() + available_width,
                                name_response.rect.bottom(),
                            ),
                        );
                        let row_response = tui.egui_ui_mut().interact(
                            full_row_rect,
                            Id::new("row_sense_full").with(tab_id).with(row_index),
                            Sense::click(),
                        );

                        row_results.push(RowResult {
                            row_index,
                            response: row_response,
                        });
                    },
                );

                // ── Sticky header row (grid row 1) ───────────────────────
                // Name header
                tui.sticky([false, true].into())
                    .id(tid(("header_name", tab_id)))
                    .mut_style(|style| {
                        style.grid_row = style_helpers::line(1);
                        style.grid_column = line(1);
                        style.padding = length(4.);
                        style.align_items = Some(taffy::AlignItems::Center);
                        // Allow the Name cell to shrink with the column but
                        // never below zero; text uses Extend so it is never
                        // clipped to "…" regardless of the measured width.
                        style.min_size.width = length(0.0);
                        style.overflow.x = taffy::Overflow::Hidden;
                    })
                    .add_with_background_color(|tui| {
                        tui.mut_style(|style| {
                            style.size.width = percent(1.);
                            style.overflow.x = taffy::Overflow::Hidden;
                        })
                        .ui(|ui: &mut Ui| {
                            let res = ui.add(
                                egui::Label::new("Name")
                                    // Extend keeps the full text visible even
                                    // when the column is narrow; the parent
                                    // cell clips it via overflow:hidden.
                                    .wrap_mode(egui::TextWrapMode::Extend)
                                    .selectable(false)
                                    .sense(Sense::click()),
                            );
                            if res.clicked() {
                                new_sort = Some(Sort::Name);
                            }
                        });
                    });

                // Modified header
                tui.sticky([false, true].into())
                    .id(tid(("header_modified", tab_id)))
                    .mut_style(|style| {
                        style.grid_row = style_helpers::line(1);
                        style.grid_column = line(2);
                        style.padding = length(4.);
                        style.align_items = Some(taffy::AlignItems::Center);
                        style.justify_content = Some(taffy::JustifyContent::FlexEnd);
                        style.size = taffy::Size {
                            width: length(COL_MODIFIED_W),
                            height: auto(),
                        };
                        style.min_size = taffy::Size {
                            width: length(COL_MODIFIED_MIN),
                            height: auto(),
                        };
                        style.max_size = taffy::Size {
                            width: length(COL_MODIFIED_MAX),
                            height: auto(),
                        };
                    })
                    .add_with_background_color(|tui| {
                        tui.mut_style(|style| {
                            style.size.width = percent(1.);
                        })
                        .ui(|ui: &mut Ui| {
                            ui.with_layout(Layout::right_to_left(egui::Align::Center), |ui| {
                                let res = ui.add(
                                    egui::Label::new("Modified")
                                        .wrap_mode(egui::TextWrapMode::Extend)
                                        .selectable(false)
                                        .sense(Sense::click()),
                                );
                                if res.clicked() {
                                    new_sort = Some(Sort::Modified);
                                }
                            });
                        });
                    });

                // Size header
                tui.sticky([false, true].into())
                    .id(tid(("header_size", tab_id)))
                    .mut_style(|style| {
                        style.grid_row = style_helpers::line(1);
                        style.grid_column = line(3);
                        style.padding = length(4.);
                        style.align_items = Some(taffy::AlignItems::Center);
                        style.justify_content = Some(taffy::JustifyContent::FlexEnd);
                        style.size = taffy::Size {
                            width: length(COL_SIZE_W),
                            height: auto(),
                        };
                        style.min_size = taffy::Size {
                            width: length(COL_SIZE_MIN),
                            height: auto(),
                        };
                        style.max_size = taffy::Size {
                            width: length(COL_SIZE_MAX),
                            height: auto(),
                        };
                    })
                    .add_with_background_color(|tui| {
                        tui.mut_style(|style| {
                            style.size.width = percent(1.);
                        })
                        .ui(|ui: &mut Ui| {
                            ui.with_layout(Layout::right_to_left(egui::Align::Center), |ui| {
                                let res = ui.add(
                                    egui::Label::new("Size")
                                        .wrap_mode(egui::TextWrapMode::Extend)
                                        .selectable(false)
                                        .sense(Sense::click()),
                                );
                                if res.clicked() {
                                    new_sort = Some(Sort::Size);
                                }
                            });
                        });
                    });
            });
        });

        // ── Process row interactions (post-tui, tab borrow is free again) ────
        for RowResult {
            row_index,
            response: row_response,
        } in &row_results
        {
            let row_index = *row_index;
            let index = tab.visible_entries[row_index];
            let val = &tab.list[index];
            let is_dir = !val.is_file();
            let indexed = row_index + 1;

            if row_response.double_clicked()
                || convert_nr_to_egui_key(indexed)
                    .is_some_and(|k| row_response.ctx.key_with_command_pressed(k))
            {
                if val.is_file() {
                    ActionToPerform::SystemOpen(val.path.clone().into()).schedule();
                } else if let Ok(path) = std::fs::canonicalize(val.get_path()) {
                    if row_response.ctx.shift_pressed() {
                        ActionToPerform::NewTab(path).schedule();
                    } else {
                        TabAction::ChangePaths(path.into()).schedule_tab(tab_id);
                    }
                }
            } else if row_response.clicked() {
                if !shift_pressed {
                    selected_tabs.selected_fields.clear();
                }
                selected_tabs.selected_fields.push(row_index);
                selected_tabs.just_changed = true;
                row_response
                    .ctx
                    .data_set_path(&tab.current_path, selected_tabs.clone());
            }

            // ── Context menu ─────────────────────────────────────────────────
            let popup_id = tab_popup_id.with(row_index);
            egui::Popup::context_menu(row_response)
                .id(popup_id)
                .show(|ui| {
                    ui.data_set_tab::<PopupOpened>(tab_id, PopupOpened(popup_id, row_index));
                    let options = build_for_path(&tab.current_path, val.get_path(), &favorites);
                    for option in options {
                        if ui.button(option.name.as_str()).clicked() {
                            option.action.clone().schedule();
                            ui.close();
                        }
                    }
                    if !is_dir {
                        if ui.button("Open").clicked() {
                            ActionToPerform::SystemOpen(val.path.clone().into()).schedule();
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
                        let matching_dirs = self.tab_paths.iter().filter(|s| !val_dir.eq(*s));
                        #[allow(clippy::collapsible_else_if)]
                        if matching_dirs.clone().count() > 0 {
                            ui.separator();
                            ui.menu_button("Move to", |ui| {
                                for other in matching_dirs {
                                    let other = PathBuf::from(
                                        std::fs::canonicalize(other)
                                            .unwrap_or_else(|_| val.get_path().to_path_buf())
                                            .to_fixed_string(),
                                    );

                                    if ui
                                        .button(format!(
                                            "{}",
                                            &other.file_name().expect("Failed").to_string_lossy()
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
                                                if *nr == 17 {
                                                    let copy_success =
                                                        fs::copy(path, target_path.clone()).is_ok();
                                                    if copy_success {
                                                        success = fs::remove_file(path).is_ok();
                                                    }
                                                }
                                            });
                                        });
                                        if success {
                                            crate::app::database::invalidate_dir(&val_dir);
                                            crate::app::database::invalidate_dir(&other);
                                            crate::app::commands::COMMANDS_QUEUE.push(
                                                ActionToPerform::TabAction(
                                                    TabTarget::AllTabs,
                                                    TabAction::RequestFilesRefresh,
                                                ),
                                            );
                                        } else {
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
                        let parent = PathBuf::from(val.get_splitted_path().0);
                        trash::delete(val.get_path()).unwrap_or_else(|_| {
                            toast!(Error, "Could not move it to trash.");
                        });
                        crate::app::database::invalidate_dir(&parent);
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

            // Scroll selected row into view
            if just_changed
                && let Some(v) = selected_tabs.selected_fields.last()
                && *v == row_index
            {
                ui.scroll_to_cursor(Some(egui::Align::Center));
            }
        }

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
            let visible_len = tab.visible_entries.len();
            if visible_len == 0 {
                return;
            }
            let new_value = match selected_tabs.selected_fields.last() {
                Some(i) => match change {
                    egui::Key::ArrowDown => i.saturating_add(1).min(visible_len.saturating_sub(1)),
                    egui::Key::ArrowUp => i.saturating_sub(1),
                    egui::Key::ArrowLeft => {
                        if let Some(parent) = tab.current_path.parent() {
                            TabAction::ChangePaths(parent.into()).schedule_tab(tab.id);
                        }
                        return;
                    }
                    egui::Key::Enter | egui::Key::ArrowRight => {
                        let Some(entry) = tab
                            .visible_entries
                            .get(*i)
                            .and_then(|visible| tab.list.get(*visible))
                        else {
                            return;
                        };
                        Self::activate_entry(entry, tab_id, false);
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

    #[allow(clippy::too_many_arguments)]
    fn draw_grid_tile(
        &mut self,
        ui: &mut Ui,
        entry: &DirEntry,
        tile_width: f32,
        tile_height: f32,
        is_selected: bool,
        is_popup_open: bool,
        cmd: bool,
        indexed: usize,
    ) -> egui::Response {
        let (rect, response) =
            ui.allocate_exact_size(Vec2::new(tile_width, tile_height), Sense::click());
        let visuals = &ui.style().visuals;
        let hovered = response.hovered() || is_popup_open;
        let bg_fill = if is_selected {
            visuals.selection.bg_fill
        } else if hovered {
            visuals.widgets.hovered.bg_fill
        } else {
            visuals.faint_bg_color
        };
        let stroke = if is_selected {
            visuals.selection.stroke
        } else if hovered {
            visuals.widgets.hovered.bg_stroke
        } else {
            visuals.widgets.noninteractive.bg_stroke
        };

        ui.painter().rect_filled(rect, 8.0, bg_fill);
        ui.painter()
            .rect_stroke(rect, 8.0, stroke, egui::StrokeKind::Inside);

        let inner_rect = rect.shrink2(Vec2::splat(GRID_TILE_PADDING));
        let text_width = inner_rect.width().max(0.0);
        let mut child_ui = ui.new_child(
            egui::UiBuilder::new()
                .max_rect(inner_rect)
                .layout(Layout::top_down(egui::Align::Center)),
        );
        child_ui.spacing_mut().item_spacing = Vec2::new(6.0, 6.0);

        let preview_size = GRID_TILE_PREVIEW_SIZE.max(self.assets.render_size_for(entry));
        let preview_height = (inner_rect.height() - GRID_TILE_TEXT_HEIGHT - 12.0).max(preview_size);
        let preview_width = text_width.min(preview_size);

        child_ui.allocate_ui_with_layout(
            Vec2::new(text_width, preview_height),
            Layout::top_down_justified(egui::Align::Center),
            |ui| {
                ui.add_space(((preview_height - preview_size) * 0.5).max(0.0));
                if let Some(texture) = self.assets.request_entry_texture(entry) {
                    ui.add(
                        egui::Image::new(&texture)
                            .maintain_aspect_ratio(true)
                            .fit_to_exact_size(Vec2::new(preview_width, preview_size)),
                    );
                } else {
                    ui.allocate_space(Vec2::new(preview_width, preview_size));
                }
            },
        );

        if cmd && indexed < 10 {
            child_ui.add(
                egui::Label::new(LayoutJob::simple_format(
                    format!("[{indexed}]"),
                    TextFormat {
                        color: Color32::from_gray(110),
                        ..Default::default()
                    },
                ))
                .wrap_mode(egui::TextWrapMode::Truncate)
                .selectable(false)
                .sense(Sense::empty()),
            );
        }

        child_ui.add_sized(
            [text_width, GRID_TILE_TEXT_HEIGHT],
            egui::Label::new(entry.get_splitted_path().1)
                .wrap_mode(egui::TextWrapMode::Truncate)
                .selectable(false)
                .sense(Sense::empty()),
        );

        response.on_hover_ui(|ui| Self::show_entry_hover_preview(self.assets, ui, entry))
    }

    #[allow(clippy::too_many_arguments)]
    fn process_grid_interactions(
        &mut self,
        ui: &Ui,
        tab: &TabData,
        selected_tabs: &mut Selected,
        shift_pressed: bool,
        just_changed: bool,
        tab_popup_id: Id,
        favorites: &Locations,
        row_results: &[RowResult],
    ) {
        for RowResult {
            row_index,
            response: row_response,
        } in row_results
        {
            let row_index = *row_index;
            let Some(index) = tab.visible_entries.get(row_index).copied() else {
                continue;
            };
            let val = &tab.list[index];
            let indexed = row_index + 1;

            if row_response.double_clicked()
                || convert_nr_to_egui_key(indexed)
                    .is_some_and(|key| row_response.ctx.key_with_command_pressed(key))
            {
                Self::activate_entry(val, tab.id, row_response.ctx.shift_pressed());
            } else if row_response.clicked() {
                if !shift_pressed {
                    selected_tabs.selected_fields.clear();
                }
                if !selected_tabs.selected_fields.contains(&row_index) {
                    selected_tabs.selected_fields.push(row_index);
                }
                selected_tabs.just_changed = true;
                row_response
                    .ctx
                    .data_set_path(&tab.current_path, selected_tabs.clone());
            }

            self.show_entry_context_menu(
                row_response,
                tab,
                val,
                row_index,
                tab_popup_id,
                favorites,
            );

            if just_changed
                && let Some(v) = selected_tabs.selected_fields.last()
                && *v == row_index
            {
                ui.scroll_to_rect(row_response.rect, Some(egui::Align::Center));
            }
        }
    }

    #[allow(clippy::too_many_lines)]
    fn show_entry_context_menu(
        &self,
        row_response: &egui::Response,
        tab: &TabData,
        val: &DirEntry,
        row_index: usize,
        tab_popup_id: Id,
        favorites: &Locations,
    ) {
        let is_dir = !val.is_file();
        let popup_id = tab_popup_id.with(row_index);
        egui::Popup::context_menu(row_response)
            .id(popup_id)
            .show(|ui| {
                ui.data_set_tab::<PopupOpened>(tab.id, PopupOpened(popup_id, row_index));
                let options = build_for_path(&tab.current_path, val.get_path(), favorites);
                for option in options {
                    if ui.button(option.name.as_str()).clicked() {
                        option.action.clone().schedule();
                        ui.close();
                    }
                }
                if !is_dir {
                    if ui.button("Open").clicked() {
                        ActionToPerform::SystemOpen(val.path.clone().into()).schedule();
                        ui.close();
                        return;
                    }
                    #[cfg(windows)]
                    if ui.button("Show in explorer").clicked() {
                        crate::windows_tools::display_in_explorer(val.get_path()).unwrap_or_else(
                            |_| {
                                toast!(Error, "Could not open in explorer");
                            },
                        );
                        ui.close();
                    }
                    let val_dir = PathBuf::from(val.get_splitted_path().0);
                    let matching_dirs = self.tab_paths.iter().filter(|path| !val_dir.eq(*path));
                    #[allow(clippy::collapsible_else_if)]
                    if matching_dirs.clone().count() > 0 {
                        ui.separator();
                        ui.menu_button("Move to", |ui| {
                            for other in matching_dirs {
                                let other = PathBuf::from(
                                    std::fs::canonicalize(other)
                                        .unwrap_or_else(|_| val.get_path().to_path_buf())
                                        .to_fixed_string(),
                                );

                                if ui
                                    .button(format!(
                                        "{}",
                                        &other.file_name().expect("Failed").to_string_lossy()
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
                                            if *nr == 17 {
                                                let copy_success =
                                                    fs::copy(path, target_path.clone()).is_ok();
                                                if copy_success {
                                                    success = fs::remove_file(path).is_ok();
                                                }
                                            }
                                        });
                                    });
                                    if success {
                                        crate::app::database::invalidate_dir(&val_dir);
                                        crate::app::database::invalidate_dir(&other);
                                        crate::app::commands::COMMANDS_QUEUE.push(
                                            ActionToPerform::TabAction(
                                                TabTarget::AllTabs,
                                                TabAction::RequestFilesRefresh,
                                            ),
                                        );
                                    } else {
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
                    let parent = PathBuf::from(val.get_splitted_path().0);
                    trash::delete(val.get_path()).unwrap_or_else(|_| {
                        toast!(Error, "Could not move it to trash.");
                    });
                    crate::app::database::invalidate_dir(&parent);
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
    }

    fn handle_grid_keyboard_navigation(
        &self,
        ui: &Ui,
        tab: &TabData,
        selected_tabs: &mut Selected,
        shift_pressed: bool,
        columns: usize,
    ) {
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
            } else if i.key_pressed(egui::Key::ArrowRight) {
                Some(egui::Key::ArrowRight)
            } else if i.key_pressed(egui::Key::Enter) {
                Some(egui::Key::Enter)
            } else {
                None
            }
        });

        let visible_len = tab.visible_entries.len();
        if visible_len == 0 {
            return;
        }

        let Some(change) = input_key else {
            return;
        };
        let current = selected_tabs
            .selected_fields
            .last()
            .copied()
            .filter(|index| *index < visible_len)
            .unwrap_or_default();
        let new_value = match change {
            egui::Key::ArrowDown => current.saturating_add(columns).min(visible_len - 1),
            egui::Key::ArrowUp => current.saturating_sub(columns),
            egui::Key::ArrowLeft => current.saturating_sub(1),
            egui::Key::ArrowRight => current.saturating_add(1).min(visible_len - 1),
            egui::Key::Enter => {
                let Some(entry) = tab
                    .visible_entries
                    .get(current)
                    .and_then(|visible| tab.list.get(*visible))
                else {
                    return;
                };
                Self::activate_entry(entry, tab.id, false);
                current
            }
            _ => return,
        };

        if !shift_pressed {
            selected_tabs.selected_fields.clear();
        }
        if !selected_tabs.selected_fields.contains(&new_value) {
            selected_tabs.selected_fields.push(new_value);
        }
        selected_tabs.just_changed = true;
        ui.data_set_path(&tab.current_path, selected_tabs.clone());
    }

    fn activate_entry(entry: &DirEntry, tab_id: u32, open_in_new_tab: bool) {
        if entry.is_file() {
            ActionToPerform::SystemOpen(entry.path.clone().into()).schedule();
        } else if let Ok(path) = std::fs::canonicalize(entry.get_path()) {
            if open_in_new_tab {
                ActionToPerform::NewTab(path).schedule();
            } else {
                TabAction::ChangePaths(path.into()).schedule_tab(tab_id);
            }
        }
    }

    fn show_entry_hover_preview(assets: &mut AssetManager, ui: &mut Ui, entry: &DirEntry) {
        match assets.request_hover_preview(entry) {
            HoverPreview::ImageUri(path) => {
                ui.add(
                    egui::Image::new(path)
                        .maintain_aspect_ratio(true)
                        .max_size(Vec2::new(300.0, 300.0)),
                );
            }
            HoverPreview::GifBytes { uri, bytes } => {
                ui.add(
                    egui::Image::from_bytes(uri, bytes)
                        .maintain_aspect_ratio(true)
                        .max_size(Vec2::new(300.0, 300.0)),
                );
            }
            HoverPreview::Loading => {
                ui.label("Loading preview...");
                ui.label(entry.path.as_str());
            }
            HoverPreview::Fallback => {
                ui.label(entry.path.as_str());
            }
        }
    }
}

impl TabViewer for MyTabViewer<'_> {
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
        puffin::profile_scope!("lwa_fm::MyTabViewer::ui");
        match tab.display_type {
            DisplayType::List => self.list_view(ui, tab),
            DisplayType::Icons => self.grid_view(ui, tab),
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
            TabTarget::AllTabs => None,
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
    #[inline]
    pub fn get_tab_ids(&mut self) -> Vec<u32> {
        self.dock_state
            .iter_all_tabs_mut()
            .map(|(_, tab)| tab.id)
            .collect()
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

    pub fn ui(&mut self, ui: &mut Ui, assets: &mut AssetManager) {
        #[cfg(feature = "profiling")]
        puffin::profile_scope!("lwa_fm::MyTabs::ui");
        let tabs = self.get_tabs_paths();
        let tabs_len = self.dock_state.iter_all_tabs().count();
        let Some(active_tab) = self.get_current_index() else {
            return;
        };
        // {
        //     #[cfg(feature = "profiling")]
        //     puffin::profile_scope!("lwa_fm::MyTabs::ui::check_for_file_system_events");
        //     for ((_, _), item) in self.dock_state.iter_all_tabs_mut() {
        //         if item.check_for_file_system_events() {
        //             TabAction::RequestFilesRefresh.schedule_tab(item.id);
        //         }
        //     }
        // }
        let mut my_tab_viewer = MyTabViewer {
            closeable: tabs_len > 1,
            tab_paths: tabs,
            active_tab,
            focused: self.focused,
            assets,
        };
        ui.spacing_mut().item_spacing = [0.0, 0.0].into();
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
                .set_focused_node_and_surface(egui_dock::NodePath::MAIN_ROOT);
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
        style.tab.tab_body.inner_margin = egui::Margin::same(0);
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::data::files::{DirEntry, EntryType};
    use crate::helper::DataHolder;
    use std::sync::OnceLock;

    /// Build a list of [`DirEntry`] values from the project's `src/` directory.
    ///
    /// Using real filesystem entries means paths, timestamps, and sizes are all
    /// realistic, which exercises the galley-pool paths in the renderer.
    fn make_entries() -> Vec<DirEntry> {
        std::fs::read_dir("src")
            .expect("src/ must exist")
            .filter_map(|e| e.ok())
            .filter_map(|e| DirEntry::try_from(e).ok())
            .take(20)
            .collect()
    }

    fn fixture_root() -> &'static std::path::Path {
        static ROOT: OnceLock<std::path::PathBuf> = OnceLock::new();
        ROOT.get_or_init(|| {
            let root = std::env::temp_dir().join("lwa_fm_kittest_grid");
            std::fs::create_dir_all(&root).expect("grid fixture root must be created");

            for dir_name in ["Applications", "Projects", "Pictures"] {
                std::fs::create_dir_all(root.join(dir_name))
                    .expect("grid fixture directory must be created");
            }

            for (name, bytes) in [
                (
                    "Zrzut ekranu 2026-03-16 o 12.45.04.png",
                    &MINI_PNG[..],
                ),
                (
                    "Zrzut ekranu 2026-03-09 o 13.14.58.png",
                    &MINI_PNG[..],
                ),
                (
                    "Vacation panorama with a very descriptive title.png",
                    &MINI_PNG[..],
                ),
            ] {
                std::fs::write(root.join(name), bytes).expect("png fixture must be written");
            }

            for name in [
                "Old World.app",
                "Factorio save data.zip",
                "Meeting notes.txt",
                "Quarterly financial summary.pdf",
                "README.md",
                "Recording archive.rs",
                "super-extra-long filename that should truncate gracefully in the icon grid view.txt",
            ] {
                std::fs::write(root.join(name), b"fixture").expect("text fixture must be written");
            }

            root
        })
        .as_path()
    }

    const MINI_PNG: [u8; 70] = [
        137, 80, 78, 71, 13, 10, 26, 10, 0, 0, 0, 13, 73, 72, 68, 82, 0, 0, 0, 1, 0, 0, 0, 1, 8, 6,
        0, 0, 0, 31, 21, 196, 137, 0, 0, 0, 11, 73, 68, 65, 84, 120, 218, 99, 252, 255, 159, 161,
        30, 0, 7, 130, 2, 127, 60, 211, 91, 198, 0, 0, 0, 0, 73, 69, 78, 68, 174, 66, 96, 130,
    ];

    fn grid_entries() -> Vec<DirEntry> {
        let mut entries: Vec<_> = std::fs::read_dir(fixture_root())
            .expect("grid fixture root must exist")
            .filter_map(|entry| entry.ok())
            .filter_map(|entry| DirEntry::try_from(entry).ok())
            .collect();
        entries.sort_by(|left, right| left.get_splitted_path().1.cmp(right.get_splitted_path().1));
        entries
    }

    fn long_name_entries() -> Vec<DirEntry> {
        let mut entries = grid_entries();
        entries.sort_by(|left, right| {
            right
                .get_splitted_path()
                .1
                .len()
                .cmp(&left.get_splitted_path().1.len())
        });
        entries.truncate(9);
        entries
    }

    fn populated_tabs_from_entries(entries: Vec<DirEntry>, display_type: DisplayType) -> MyTabs {
        let path = fixture_root();
        let mut tab = TabData::from_path(path);
        tab.display_type = display_type;
        tab.list = entries;
        tab.visible_entries = (0..tab.list.len()).collect();
        MyTabs {
            dock_state: egui_dock::DockState::new(vec![tab]),
            focused: true,
        }
    }

    /// Build a [`MyTabs`] whose active tab is populated from `src/`.
    fn populated_tabs() -> MyTabs {
        let path = std::path::Path::new("src");
        let mut tab = TabData::from_path(path);
        tab.list = make_entries();
        tab.visible_entries = (0..tab.list.len()).collect();
        MyTabs {
            dock_state: egui_dock::DockState::new(vec![tab]),
            focused: true,
        }
    }

    /// Draw one frame of the dock view, seeding galley pools and draining commands.
    fn draw_frame(ctx: &egui::Context, my_tabs: &mut MyTabs) {
        draw_frame_with_selection(ctx, my_tabs, None);
    }

    fn draw_frame_with_selection(
        ctx: &egui::Context,
        my_tabs: &mut MyTabs,
        selected_fields: Option<&[usize]>,
    ) {
        let mut assets = crate::app::assets::AssetManager::default();
        // Seed the pre-computed galley pools so time / size widgets render.
        for (_, tab) in my_tabs.dock_state.iter_all_tabs() {
            populate_time_pool(tab.list.iter().map(|e| e.meta.since_modified), ctx);
            populate_sizes_pool(tab.list.iter().map(|e| e.meta.size), ctx);
        }

        egui::CentralPanel::default().show(ctx, |ui| {
            if let Some(indices) = selected_fields {
                let current_path = my_tabs
                    .get_current_tab()
                    .expect("missing active tab")
                    .current_path
                    .clone();
                ui.data_set_path(
                    &current_path,
                    Selected {
                        selected_fields: indices.to_vec(),
                        just_changed: false,
                    },
                );
            }
            my_tabs.ui(ui, &mut assets);
        });

        // Drain the command queue so it does not fill up across frames.
        while crate::app::commands::COMMANDS_QUEUE.pop().is_some() {}
    }

    // ── Snapshot: dock view at a standard desktop width ──────────────────────

    #[test]
    fn test_dock_view_wide() {
        let mut harness = egui_kittest::Harness::builder()
            .with_size(egui::Vec2::new(900.0, 500.0))
            .build_state(
                |ctx, my_tabs: &mut MyTabs| draw_frame(ctx, my_tabs),
                populated_tabs(),
            );

        harness.run_steps(12);
        harness.snapshot("dock_view_wide");
    }

    // ── Snapshot: dock view at a narrow width (split-pane scenario) ───────────
    //
    // Before the fix the name column had `min_size.width = 260 px` and the grid
    // had `overflow.x = Visible`, which forced cells to bleed outside the panel
    // at this width.  The snapshot now shows text clipped with "…" instead.

    #[test]
    fn test_dock_view_narrow() {
        let mut harness = egui_kittest::Harness::builder()
            .with_size(egui::Vec2::new(350.0, 500.0))
            .build_state(
                |ctx, my_tabs: &mut MyTabs| draw_frame(ctx, my_tabs),
                populated_tabs(),
            );

        harness.run_steps(12);
        harness.snapshot("dock_view_narrow");
    }

    // ── Snapshot: dock view with long / duplicated names (ellipsis stress) ────

    #[test]
    fn test_dock_view_long_names() {
        // Duplicate entries so we get more rows and more truncation to render.
        let entries = make_entries();
        let all: Vec<DirEntry> = entries
            .iter()
            .chain(entries.iter())
            .cloned()
            .take(20)
            .collect();

        let path = std::path::Path::new("src");
        let mut tab = TabData::from_path(path);
        tab.list = all;
        tab.visible_entries = (0..tab.list.len()).collect();
        let my_tabs = MyTabs {
            dock_state: egui_dock::DockState::new(vec![tab]),
            focused: false,
        };

        let mut harness = egui_kittest::Harness::builder()
            .with_size(egui::Vec2::new(400.0, 600.0))
            .build_state(
                |ctx, my_tabs: &mut MyTabs| draw_frame(ctx, my_tabs),
                my_tabs,
            );

        harness.run_steps(12);
        harness.snapshot("dock_view_long_names");
    }

    // ── Snapshot: empty tab (no entries) ─────────────────────────────────────

    #[test]
    fn test_dock_view_empty_tab() {
        let tab = TabData::from_path(std::path::Path::new("src"));
        let my_tabs = MyTabs {
            dock_state: egui_dock::DockState::new(vec![tab]),
            focused: false,
        };

        let mut harness = egui_kittest::Harness::builder()
            .with_size(egui::Vec2::new(700.0, 400.0))
            .build_state(
                |ctx, my_tabs: &mut MyTabs| draw_frame(ctx, my_tabs),
                my_tabs,
            );

        harness.run_steps(12);
        harness.snapshot("dock_view_empty_tab");
    }

    #[test]
    fn test_dock_view_icons_wide() {
        let mut harness = egui_kittest::Harness::builder()
            .with_size(egui::Vec2::new(960.0, 620.0))
            .build_state(
                |ctx, my_tabs: &mut MyTabs| draw_frame(ctx, my_tabs),
                populated_tabs_from_entries(grid_entries(), DisplayType::Icons),
            );

        harness.run_steps(12);
        harness.snapshot("dock_view_icons_wide");
    }

    #[test]
    fn test_dock_view_icons_narrow() {
        let mut harness = egui_kittest::Harness::builder()
            .with_size(egui::Vec2::new(420.0, 620.0))
            .build_state(
                |ctx, my_tabs: &mut MyTabs| draw_frame(ctx, my_tabs),
                populated_tabs_from_entries(grid_entries(), DisplayType::Icons),
            );

        harness.run_steps(12);
        harness.snapshot("dock_view_icons_narrow");
    }

    #[test]
    fn test_dock_view_icons_long_names() {
        let mut harness = egui_kittest::Harness::builder()
            .with_size(egui::Vec2::new(620.0, 700.0))
            .build_state(
                |ctx, my_tabs: &mut MyTabs| draw_frame(ctx, my_tabs),
                populated_tabs_from_entries(long_name_entries(), DisplayType::Icons),
            );

        harness.run_steps(12);
        harness.snapshot("dock_view_icons_long_names");
    }

    #[test]
    fn test_dock_view_icons_empty_tab() {
        let mut tab = TabData::from_path(fixture_root());
        tab.display_type = DisplayType::Icons;
        let my_tabs = MyTabs {
            dock_state: egui_dock::DockState::new(vec![tab]),
            focused: false,
        };

        let mut harness = egui_kittest::Harness::builder()
            .with_size(egui::Vec2::new(720.0, 420.0))
            .build_state(
                |ctx, my_tabs: &mut MyTabs| draw_frame(ctx, my_tabs),
                my_tabs,
            );

        harness.run_steps(12);
        harness.snapshot("dock_view_icons_empty_tab");
    }

    #[test]
    fn test_dock_view_icons_selected() {
        let mut harness = egui_kittest::Harness::builder()
            .with_size(egui::Vec2::new(960.0, 620.0))
            .build_state(
                |ctx, my_tabs: &mut MyTabs| draw_frame_with_selection(ctx, my_tabs, Some(&[2])),
                populated_tabs_from_entries(grid_entries(), DisplayType::Icons),
            );

        harness.run_steps(12);
        harness.snapshot("dock_view_icons_selected");
    }

    // ── Unit: DirEntry constructed from real fs has correct entry type ────────

    #[test]
    fn test_dir_entry_entry_type() {
        let entries = make_entries();
        assert!(!entries.is_empty(), "src/ must contain at least one entry");
        for entry in &entries {
            let path = entry.get_path();
            if path.is_file() {
                assert!(
                    entry.is_file(),
                    "Expected file entry for {}",
                    path.display()
                );
            } else {
                assert!(
                    !entry.is_file(),
                    "Expected directory entry for {}",
                    path.display()
                );
                assert_eq!(
                    entry.meta.entry_type,
                    EntryType::Directory,
                    "Entry type mismatch for {}",
                    path.display()
                );
            }
        }
    }
}
