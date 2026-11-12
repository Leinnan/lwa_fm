//! Details (list) view of a tab: virtualized rows, column header and row interaction.
use super::columns::{CELL_PADDING, Column, ListColumns};
use super::{
    MyTabViewer, RowResult, SIZES_POOL, ScrollRequest, TIME_POOL, TabData, ViewHeader,
    draw_cursor_ring, ensure_text_cache_style, file_name_galley, highlighted_name_galley,
    populate_sizes_pool, populate_time_pool, text_fit,
};
use crate::app::Sort;
use crate::app::commands::ActionToPerform;
use crate::app::directory_view_settings::DirectoryViewSettings;
use crate::data::files::EntryRef;
use crate::helper::DataHolder;
use egui::{Color32, CornerRadius, FontId, Galley, Id, Layout, Sense, Ui, Vec2};
use egui_taffy::TuiBuilderLogic;
use egui_taffy::taffy;
use egui_taffy::taffy::prelude::LengthPercentage;
use egui_taffy::taffy::prelude::LengthPercentageAuto;
use egui_taffy::taffy::prelude::Rect;
use egui_taffy::taffy::prelude::TaffyZero;
use egui_taffy::taffy::prelude::auto;
use egui_taffy::taffy::prelude::fr;
use egui_taffy::taffy::prelude::length;
use egui_taffy::taffy::prelude::line;
use egui_taffy::taffy::prelude::min_content;
use egui_taffy::taffy::prelude::minmax;
use egui_taffy::taffy::prelude::percent;
use egui_taffy::tid;
use egui_taffy::tui;
use egui_taffy::virtual_tui::VirtualGridRowHelper;
use egui_taffy::virtual_tui::VirtualGridRowHelperParams;
use std::sync::Arc;

/// Inset of the rounded hover/selection highlight from the row's ends.
const ROW_HIGHLIGHT_INSET: f32 = 4.0;
const ROW_HIGHLIGHT_RADIUS: u8 = 4;

/// The layers of a row's background: zebra stripe, highlight, and the line
/// over the first file after the folders.
#[derive(Clone, Copy)]
struct RowLook {
    stripe: Color32,
    highlight: Color32,
    divider: Option<Color32>,
}

/// What the header asked for while it was drawn, applied afterwards.
#[derive(Default)]
struct HeaderEvents {
    sort: Option<Sort>,
    resize: Option<(Column, f32)>,
    reset_width: Option<Column>,
    toggle: Option<Column>,
    reset_columns: bool,
}

/// Per-frame values every cell reads.
struct CellContext<'a> {
    highlights: Option<&'a crate::app::search::filter::Highlights>,
    highlight_variant: u64,
    scope_root: Option<&'a str>,
    font: FontId,
    text: Color32,
    weak: Color32,
}

impl MyTabViewer<'_> {
    // Keep the virtualized grid layout together, as in the icon view.
    #[allow(clippy::too_many_lines)]
    pub(super) fn list_view(&mut self, ui: &mut Ui, tab: &mut TabData) {
        ensure_text_cache_style(ui);
        // Keys first, so the moved cursor is drawn and scrolled to this frame.
        self.handle_keyboard_navigation(ui, tab, 1, tab.view.page_rows);
        let hovered_row = Self::pointer_row(ui, tab);
        let Some(header) = Self::begin_view(ui, tab) else {
            return;
        };
        let ViewHeader {
            tab_id,
            opened_popup,
            ..
        } = header;

        let in_trash = tab.current_path.is_trash();
        let with_folder = tab.is_searching() || tab.deep_or_multiple_paths();
        let highlights = tab
            .search
            .as_ref()
            .and_then(crate::app::search::filter::Highlights::new);
        // Result locations are shown relative to the searched folder.
        let scope_root = tab
            .current_path
            .single_path()
            .map(|root| root.to_string_lossy().into_owned());
        let cells = CellContext {
            highlight_variant: highlights.as_ref().map_or(0, text_fit::variant_of),
            highlights: highlights.as_ref(),
            scope_root: scope_root.as_deref(),
            font: FontId::default(),
            text: ui.visuals().text_color(),
            weak: ui.visuals().weak_text_color(),
        };
        let row_height = crate::app::ui::control_height(ui);
        let mut columns: ListColumns = ui.data_get_persisted().unwrap_or_default();
        let layout = columns.layout(ui.available_width(), with_folder);
        let last_column = layout.len() - 1;
        let template: Vec<_> = layout
            .iter()
            .map(|&(column, width)| {
                if column == Column::Name {
                    minmax(length(0.0_f32), fr(1.0_f32))
                } else {
                    length(width)
                }
            })
            .collect();

        let entries_len = tab.visible_entries.len();
        let view_settings: DirectoryViewSettings =
            ui.data_get_path_or_persisted(&tab.current_path).data;
        let mut events = HeaderEvents::default();

        let visuals = ui.visuals();
        let selection_fill = self.selection_fill(ui, tab);
        let popup_fill = visuals.widgets.hovered.bg_fill;
        // The same faint hover as grid tiles.
        let hover_fill = visuals.widgets.hovered.bg_fill.gamma_multiply(0.45);
        let stripe_fill = visuals.faint_bg_color;
        let palette = crate::app::ui::Palette::of(ui);

        // ── Unified virtual grid: sticky header row + scrollable body ────────
        //
        // One grid column per entry of `layout`: Name takes what is left,
        // the others have fixed widths. Row 1 is the sticky header; data rows
        // follow via VirtualGridRowHelper. Row interactions are collected and
        // processed after the tui closure, because `tab` is borrowed inside it.
        let header_row_count: u16 = 1;
        let mut row_results: Vec<RowResult> = Vec::with_capacity(entries_len.min(64));
        let ring_row = self.cursor_ring_row(tab);
        let mut scroll_request = header.scroll;
        // (scroll offset, header height, full rows on screen, row pitch,
        // viewport, rows area), measured from the first drawn row.
        let mut measured: Option<(f32, f32, usize, f32, f32, egui::Rect)> = None;

        tui(ui, Id::unique("file_grid").with(tab.id))
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
                size: percent(1_f32),
                max_size: percent(1_f32),
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
                    grid_template_columns: template,
                    size: taffy::Size {
                        width: percent(1_f32),
                        height: auto(),
                    },
                    max_size: percent(1_f32),
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
                            let Some(val) = tab.entry_at(row_index) else {
                                return;
                            };
                            let is_cut = self.is_cut(&val);
                            let first_file = row_index > 0
                                && val.is_file()
                                && tab
                                    .entry_at(row_index - 1)
                                    .is_some_and(|prev| !prev.is_file());
                            let look = RowLook {
                                divider: first_file.then_some(palette.border),
                                stripe: if row_index % 2 == 0 {
                                    stripe_fill
                                } else {
                                    Color32::TRANSPARENT
                                },
                                highlight: if tab.selection.contains(row_index) {
                                    selection_fill
                                } else if row_index == opened_popup {
                                    popup_fill
                                } else if hovered_row == Some(row_index) {
                                    hover_fill
                                } else {
                                    Color32::TRANSPARENT
                                },
                            };

                            let mut row_rect = egui::Rect::NOTHING;
                            for (position, &(column, _)) in layout.iter().enumerate() {
                                let ends = (position == 0, position == last_column);
                                let cell_rect =
                                    tui.id(idgen())
                                        .mut_style(&grid_row_param)
                                        .mut_style(|style| {
                                            style.padding = cell_padding();
                                            style.min_size = taffy::Size {
                                                width: length(0.0_f32),
                                                height: length(row_height),
                                            };
                                            style.overflow.x = taffy::Overflow::Hidden;
                                            style.align_items = Some(taffy::AlignItems::Center);
                                        })
                                        .add_with_background_ui(
                                            |ui, container| {
                                                let rect = container.full_container();
                                                paint_row_background(ui, rect, look, ends);
                                                rect
                                            },
                                            |tui, _| {
                                                tui.mut_style(|style| {
                                                    style.size.width = percent(1_f32);
                                                    style.overflow.x = taffy::Overflow::Hidden;
                                                })
                                                .ui(|ui: &mut Ui| {
                                                    // Cut items are dimmed across the whole row.
                                                    if is_cut {
                                                        ui.multiply_opacity(0.5);
                                                    }
                                                    self.list_cell(ui, column, &val, &cells);
                                                });
                                            },
                                        )
                                        .background;
                                row_rect = row_rect.union(cell_rect);
                            }

                            // ── Row interaction sense (full row) ──────────
                            let row_response = tui.egui_ui_mut().interact(
                                row_rect,
                                Id::unique("row_sense_full").with(tab_id).with(row_index),
                                Sense::click_and_drag(),
                            );
                            let row_response = row_response.on_hover_ui(|ui| {
                                Self::show_row_tooltip(self.assets, ui, &val, in_trash);
                            });
                            crate::app::dock::describe_entry(
                                &row_response,
                                &val,
                                look.highlight == selection_fill,
                            );
                            if ring_row == Some(row_index) {
                                draw_cursor_ring(
                                    tui.egui_ui(),
                                    row_rect.shrink2(Vec2::new(ROW_HIGHLIGHT_INSET - 2.0, -1.0)),
                                    f32::from(ROW_HIGHLIGHT_RADIUS),
                                );
                            }
                            // Any drawn row gives the geometry of all of them:
                            // rows are one height and follow the header.
                            if measured.is_none() {
                                let pitch = row_rect.height().max(1.0);
                                let viewport = tui.current_viewport();
                                let content_top = tui.current_viewport_content().top();
                                // The sticky header row sits above the first row
                                // and covers the top of the scrolled area.
                                let header_height = (row_index as f32)
                                    .mul_add(-pitch, row_rect.top() - content_top)
                                    .max(0.0);
                                let rows_area = egui::Rect::from_min_max(
                                    egui::pos2(viewport.left(), viewport.top() + header_height),
                                    viewport.max,
                                );
                                measured = Some((
                                    viewport.top() - content_top,
                                    header_height,
                                    (((viewport.height() - header_height) / pitch).floor()
                                        as usize)
                                        .saturating_sub(1)
                                        .max(1),
                                    pitch,
                                    viewport.height() - header_height,
                                    rows_area,
                                ));
                                // Scroll from inside the scroll area (the only place
                                // egui applies it), to rows drawn or not.
                                let x = row_rect.left();
                                let target = match scroll_request.take() {
                                    Some(ScrollRequest::Cursor(row)) => {
                                        let top = (row as f32)
                                            .mul_add(pitch, content_top + header_height);
                                        // Grown upward so the row is never left
                                        // under the sticky header.
                                        Some((
                                            egui::Rect::from_min_max(
                                                egui::pos2(x, top - header_height),
                                                egui::pos2(x + 1.0, top + pitch),
                                            ),
                                            None,
                                        ))
                                    }
                                    Some(ScrollRequest::Offset(offset)) => Some((
                                        egui::Rect::from_min_size(
                                            egui::pos2(x, content_top + offset),
                                            egui::vec2(1.0, viewport.height()),
                                        ),
                                        Some(egui::Align::Min),
                                    )),
                                    None => None,
                                };
                                if let Some((rect, align)) = target {
                                    tui.egui_ui().scroll_to_rect_animation(
                                        rect,
                                        align,
                                        egui::style::ScrollAnimation::none(),
                                    );
                                }
                            }

                            row_results.push(RowResult {
                                row_index,
                                response: row_response,
                            });
                        },
                    );

                    // ── Sticky header row (grid row 1) ───────────────────────
                    for (position, &(column, _)) in layout.iter().enumerate() {
                        let resize = resize_target(&layout, position);
                        let events = &mut events;
                        let columns = &columns;
                        let view_settings = &view_settings;
                        tui.sticky([false, true].into())
                            .id(tid(("header", column, tab_id)))
                            .mut_style(|style| {
                                style.grid_row = line(1);
                                style.grid_column =
                                    line(i16::try_from(position + 1).unwrap_or(i16::MAX));
                                style.padding = cell_padding();
                                style.min_size = taffy::Size {
                                    width: length(0.0_f32),
                                    height: length(row_height),
                                };
                                style.align_items = Some(taffy::AlignItems::Center);
                                style.overflow.x = taffy::Overflow::Hidden;
                            })
                            .add_with_background_ui(
                                |ui, container| {
                                    header_cell_background(
                                        ui,
                                        container.full_container(),
                                        column,
                                        resize,
                                        position > 0,
                                        &palette,
                                        columns,
                                        events,
                                    );
                                },
                                |tui, ()| {
                                    tui.mut_style(|style| {
                                        style.size.width = percent(1_f32);
                                        style.overflow.x = taffy::Overflow::Hidden;
                                    })
                                    .ui(|ui: &mut Ui| {
                                        let layout = if column.right_aligned() {
                                            Layout::right_to_left(egui::Align::Center)
                                        } else {
                                            Layout::left_to_right(egui::Align::Center)
                                        };
                                        ui.with_layout(layout, |ui| {
                                            ui.add(
                                                egui::Label::new(sort_header(
                                                    ui,
                                                    column.title(in_trash),
                                                    column.sort(),
                                                    view_settings,
                                                ))
                                                .truncate()
                                                .selectable(false)
                                                .sense(Sense::empty()),
                                            );
                                        });
                                    });
                                },
                            );
                    }
                });
            });

        // ── Process row interactions (post-tui, tab borrow is free again) ────
        self.process_row_interactions(ui, tab, &header, &row_results);
        if let Some((scroll_y, header_height, page_rows, pitch, viewport_height, rows_area)) =
            measured
        {
            tab.view.scroll_y = scroll_y;
            tab.view.header_height = header_height;
            tab.view.page_rows = page_rows;
            tab.view.row_pitch = pitch;
            tab.view.row_gap = 0.0;
            tab.view.viewport_height = viewport_height;
            tab.view.rows_area = Some(rows_area);
            tab.view.columns = 1;
        }

        let columns_before = columns.clone();
        if let Some((column, width)) = events.resize {
            columns.set_width(column, width);
        }
        if let Some(column) = events.reset_width {
            columns.reset_width(column);
        }
        if let Some(column) = events.toggle {
            columns.toggle(column);
        }
        if events.reset_columns {
            columns = ListColumns::default();
        }
        if columns != columns_before {
            ui.data_set_persisted(columns);
        }
        if let Some(new_sort) = events.sort {
            let mut settings: DirectoryViewSettings =
                ui.data_get_path_or_persisted(&tab.current_path).data;
            settings.change_sort(new_sort);
            ui.data_set_path(&tab.current_path, settings);

            ActionToPerform::ViewSettingsChanged(crate::app::DataSource::Local).schedule();
        }
        self.quick_look_ui(ui.ctx(), tab);
    }

    /// The list row under the pointer, from where rows were last drawn and
    /// the pointer is now, so the hover highlight doesn't trail it a frame.
    fn pointer_row(ui: &Ui, tab: &TabData) -> Option<usize> {
        let area = tab.view.rows_area?;
        // Nothing is hovered while something (files, a drag selection) is dragged.
        if tab.view.row_pitch <= 0.0
            || ui.ctx().dragged_id().is_some()
            || !ui.rect_contains_pointer(area)
        {
            return None;
        }
        let pointer = ui.ctx().pointer_hover_pos()?;
        let row = ((pointer.y - area.top() + tab.view.scroll_y) / tab.view.row_pitch).floor();
        (row >= 0.0)
            .then_some(row as usize)
            .filter(|&row| row < tab.visible_entries.len())
    }

    /// The contents of one cell, inside its padding.
    fn list_cell(
        &mut self,
        ui: &mut Ui,
        column: Column,
        val: &EntryRef<'_>,
        cells: &CellContext<'_>,
    ) {
        match column {
            Column::Name => self.name_cell(ui, val, cells),
            Column::Folder => {
                let (dir, _) = val.get_splitted_path();
                let shown = relative_dir(dir, cells.scope_root);
                if !shown.is_empty() {
                    let galley = text_fit::fit_path_start(
                        ui,
                        &shown,
                        &cells.font,
                        cells.weak,
                        ui.available_width(),
                    );
                    paint_text(ui, galley, false, cells.weak);
                }
            }
            Column::Modified | Column::Created | Column::Accessed => {
                let timestamp = match column {
                    Column::Modified => val.meta.modified_at,
                    Column::Created => val.meta.created_at,
                    _ => crate::app::open_history::last_opened(
                        crate::app::open_history::of_entry(val),
                        val.meta.accessed_at,
                    ),
                };
                let galley = if *timestamp == 0 {
                    text_fit::label(
                        ui,
                        &"date_unknown",
                        || "—".to_owned(),
                        &cells.font,
                        cells.weak,
                    )
                } else {
                    let elapsed = crate::data::time::Elapsed::elapsed(timestamp);
                    populate_time_pool(std::iter::once(elapsed), ui.ctx());
                    let Some(galley) =
                        TIME_POOL.with_borrow_mut(|pool| pool.get(&elapsed).cloned())
                    else {
                        return;
                    };
                    galley
                };
                paint_text(ui, galley, column.right_aligned(), cells.weak);
            }
            Column::Kind => {
                let name = val.file_name;
                let entry_type = val.meta.entry_type;
                let extension = std::path::Path::new(name).extension();
                let galley = text_fit::label(
                    ui,
                    &("kind", extension, entry_type),
                    || super::columns::kind_label(name, entry_type),
                    &cells.font,
                    cells.weak,
                );
                paint_text(ui, galley, column.right_aligned(), cells.weak);
            }
            Column::OpenCount => {
                if let Some(record) = crate::app::open_history::of_entry(val) {
                    let galley = text_fit::label(
                        ui,
                        &("open_count", record.count),
                        || record.count.to_string(),
                        &cells.font,
                        cells.weak,
                    );
                    paint_text(ui, galley, column.right_aligned(), cells.weak);
                }
            }
            Column::Size => {
                // A package's own size is its directory record's, not its contents'.
                if val.is_file() && !val.is_package() {
                    populate_sizes_pool(std::iter::once(val.meta.size), ui.ctx());
                    if let Some(galley) =
                        SIZES_POOL.with_borrow_mut(|pool| pool.get(&val.meta.size).cloned())
                    {
                        paint_text(ui, galley, column.right_aligned(), cells.weak);
                    }
                }
            }
            // Only the native apps search file contents and read what
            // media files say of themselves.
            Column::Match | Column::Dimensions | Column::Duration => {}
        }
    }

    fn name_cell(&mut self, ui: &mut Ui, val: &EntryRef<'_>, cells: &CellContext<'_>) {
        #[cfg(feature = "profiling")]
        puffin::profile_scope!("lwa_fm::MyTabViewer::ui::table_body::first_column");
        let render_sz = self.assets.icon_size().render_px().min(24.0);
        ui.with_layout(Layout::left_to_right(egui::Align::Center), |ui| {
            if let Some(texture) = self.assets.request_entry_texture(val) {
                ui.add(egui::Image::new(&texture).fit_to_exact_size(Vec2::splat(render_sz)));
            } else {
                ui.allocate_space(Vec2::splat(render_sz));
            }
            let (_, file) = val.get_splitted_path();
            let is_dir = !val.is_file();
            let (galley, variant) = cells.highlights.map_or_else(
                || (file_name_galley(ui, file, is_dir), 0),
                |highlights| {
                    let key = text_fit::fit_key(file, cells.highlight_variant, u32::MAX);
                    let galley = text_fit::cached(key, || {
                        highlighted_name_galley(ui, file, highlights)
                            .unwrap_or_else(|| file_name_galley(ui, file, is_dir))
                    });
                    (galley, cells.highlight_variant)
                },
            );
            let galley = text_fit::fit_single_line(ui, galley, ui.available_width(), variant);
            let (rect, _) = ui.allocate_exact_size(galley.size(), Sense::empty());
            if ui.is_rect_visible(rect) {
                ui.painter().galley(rect.min, galley, cells.text);
            }
        });
    }
}

const fn cell_padding() -> Rect<LengthPercentage> {
    Rect {
        left: LengthPercentage::length(CELL_PADDING),
        right: LengthPercentage::length(CELL_PADDING),
        top: LengthPercentage::ZERO,
        bottom: LengthPercentage::ZERO,
    }
}

/// One line of text fitted to the cell, aligned left or right.
fn paint_text(ui: &mut Ui, galley: Arc<Galley>, right: bool, color: Color32) {
    let galley = text_fit::fit_single_line(ui, galley, ui.available_width(), 0);
    let layout = if right {
        Layout::right_to_left(egui::Align::Center)
    } else {
        Layout::left_to_right(egui::Align::Center)
    };
    ui.with_layout(layout, |ui| {
        let (rect, _) = ui.allocate_exact_size(galley.size(), Sense::empty());
        if ui.is_rect_visible(rect) {
            ui.painter().galley(rect.min, galley, color);
        }
    });
}

/// Paints one cell's share of the row background: the zebra stripe edge to
/// edge, and the hover/selection highlight as one rounded, inset shape
/// across the row (`ends` says whether this cell is its first or last).
fn paint_row_background(ui: &Ui, rect: egui::Rect, look: RowLook, ends: (bool, bool)) {
    if look.stripe != Color32::TRANSPARENT {
        ui.painter().rect_filled(rect, 0.0, look.stripe);
    }
    if let Some(color) = look.divider {
        ui.painter().hline(
            rect.x_range(),
            rect.top() + 0.5,
            egui::Stroke::new(1.0, color),
        );
    }
    if look.highlight == Color32::TRANSPARENT {
        return;
    }
    let (first, last) = ends;
    let mut highlight = rect.shrink2(Vec2::new(0.0, 1.0));
    if first {
        highlight.min.x += ROW_HIGHLIGHT_INSET;
    }
    if last {
        highlight.max.x -= ROW_HIGHLIGHT_INSET;
    }
    let left = if first { ROW_HIGHLIGHT_RADIUS } else { 0 };
    let right = if last { ROW_HIGHLIGHT_RADIUS } else { 0 };
    let radius = CornerRadius {
        nw: left,
        sw: left,
        ne: right,
        se: right,
    };
    ui.painter().rect_filled(highlight, radius, look.highlight);
}

/// Which column dragging the left edge of column `position` resizes, and in
/// which direction: the fixed column to its left, or (next to Name, which
/// takes the leftover space) the column itself.
fn resize_target(layout: &[(Column, f32)], position: usize) -> Option<(Column, f32)> {
    let left = layout.get(position.checked_sub(1)?)?.0;
    Some(if left == Column::Name {
        (layout[position].0, -1.0)
    } else {
        (left, 1.0)
    })
}

/// A header cell's background: fill, separators, click-to-sort, the column
/// menu and the resize handle on its left edge.
#[allow(clippy::too_many_arguments)]
fn header_cell_background(
    ui: &Ui,
    rect: egui::Rect,
    column: Column,
    resize: Option<(Column, f32)>,
    separator: bool,
    palette: &crate::app::ui::Palette,
    columns: &ListColumns,
    events: &mut HeaderEvents,
) {
    let response = ui.interact(
        rect,
        ui.scope_id().with(("header_cell", column)),
        Sense::click(),
    );
    let sortable = column.sort().is_some();
    let fill = if sortable && response.hovered() {
        palette.card_bg
    } else {
        palette.panel_bg
    };
    ui.painter().rect_filled(rect, 0.0, fill);
    ui.painter().hline(
        rect.x_range(),
        rect.bottom() - 0.5,
        egui::Stroke::new(1.0, palette.border),
    );
    if sortable {
        let response = response.on_hover_cursor(egui::CursorIcon::PointingHand);
        if response.clicked() {
            events.sort = column.sort();
        }
        header_menu(&response, columns, events);
    } else {
        header_menu(&response, columns, events);
    }

    if !separator {
        return;
    }
    let mut separator_stroke = egui::Stroke::new(1.0, palette.border);
    if let Some((target, direction)) = resize {
        let handle = egui::Rect::from_center_size(
            egui::pos2(rect.left(), rect.center().y),
            egui::vec2(8.0, rect.height()),
        );
        let handle = ui
            .interact(
                handle,
                ui.scope_id().with(("column_resize", column)),
                Sense::click_and_drag(),
            )
            .on_hover_cursor(egui::CursorIcon::ResizeHorizontal);
        if handle.hovered() || handle.dragged() {
            separator_stroke = egui::Stroke::new(2.0, palette.accent);
        }
        if handle.dragged() {
            ui.ctx().set_cursor_icon(egui::CursorIcon::ResizeHorizontal);
            let delta = handle.drag_delta().x;
            if delta != 0.0 {
                events.resize = Some((target, direction.mul_add(delta, columns.width(target))));
            }
        }
        if handle.double_clicked() {
            events.reset_width = Some(target);
        }
    }
    let inset = rect.height() * 0.25;
    ui.painter().vline(
        rect.left(),
        egui::Rangef::new(rect.top() + inset, rect.bottom() - inset),
        separator_stroke,
    );
}

/// The header's context menu: show or hide columns, reset their widths.
fn header_menu(response: &egui::Response, columns: &ListColumns, events: &mut HeaderEvents) {
    response.context_menu(|ui| {
        // Only the native apps fill these two.
        let offered = Column::OPTIONAL
            .into_iter()
            .filter(|column| !matches!(column, Column::Dimensions | Column::Duration));
        for column in offered {
            let mut shown = columns.is_shown(column);
            if ui.checkbox(&mut shown, column.title(false)).clicked() {
                events.toggle = Some(column);
                ui.close();
            }
        }
        ui.separator();
        if ui.button("Reset columns").clicked() {
            events.reset_columns = true;
            ui.close();
        }
    });
}

/// `dir` relative to the searched folder (`sub\deeper\`), or the full
/// path when it is outside it. Empty for items directly in the folder.
pub(super) fn relative_dir(dir: &str, root: Option<&str>) -> String {
    let Some(root) = root else {
        return dir.to_owned();
    };
    // Compare with one separator style; Windows accepts both.
    let normalize = |path: &str| path.replace('\\', "/");
    let dir_norm = normalize(dir);
    let root_norm = normalize(root);
    let root_norm = root_norm.trim_end_matches('/');
    dir_norm.strip_prefix(root_norm).map_or_else(
        || dir.to_owned(),
        |rest| {
            rest.trim_start_matches('/')
                .replace('/', std::path::MAIN_SEPARATOR_STR)
        },
    )
}

/// A column header with an arrow when the list is sorted by that column.
fn sort_header(
    ui: &Ui,
    label: &str,
    column: Option<Sort>,
    view: &DirectoryViewSettings,
) -> egui::text::LayoutJob {
    let font = egui::TextStyle::Body.resolve(ui.style());
    let color = ui.visuals().text_color();
    let mut job = egui::text::LayoutJob::default();
    job.append(label, 0.0, egui::TextFormat::simple(font, color));
    let lucide = egui::FontFamily::Name("lucide".into());
    let has_icons = ui.fonts_mut(|fonts| fonts.families().contains(&lucide));
    if column == Some(view.sorting) && has_icons {
        let icon = if view.invert_sort {
            lucide_icons::Icon::ArrowDown
        } else {
            lucide_icons::Icon::ArrowUp
        };
        job.append(
            &char::from(icon).to_string(),
            4.0,
            egui::TextFormat::simple(
                egui::FontId::new(12.0, lucide),
                ui.visuals().weak_text_color(),
            ),
        );
    }
    job
}

#[cfg(test)]
mod tests {
    use super::relative_dir;

    #[test]
    fn result_folders_are_relative_to_the_searched_folder() {
        let sep = std::path::MAIN_SEPARATOR_STR;
        assert_eq!(
            relative_dir(r"C:\work\a\b", Some(r"C:\work")),
            format!("a{sep}b")
        );
        assert_eq!(relative_dir("/work/a", Some(r"\work\")), "a");
        assert_eq!(relative_dir(r"C:\work", Some(r"C:\work\")), "");
        assert_eq!(relative_dir(r"D:\other", Some(r"C:\work")), r"D:\other");
        assert_eq!(relative_dir("/x/y", None), "/x/y");
    }
}
