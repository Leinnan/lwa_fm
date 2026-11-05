//! Icon (grid) view of a tab: tile layout, previews, mouse and keyboard handling.
use super::{
    GRID_TILE_PADDING, GRID_VIEW_PADDING, GridGeometry, MyTabViewer, RowResult, ScrollRequest,
    TabData, describe_entry, draw_cursor_ring, text_fit, tile_preview_intent,
};
use crate::app::assets::IconSize;
use crate::app::assets::Preview;
use crate::app::assets::PreviewIntent;
use crate::app::assets::entry_has_animated_preview;
use crate::data::files::EntryRef;
use egui::Color32;
use egui::FontId;
use egui::Id;
use egui::Sense;
use egui::Ui;
use egui::Vec2;

/// Space between the preview and the name.
const PREVIEW_NAME_GAP: f32 = 4.0;
/// Space between tiles, both ways.
const TILE_GAP: f32 = 8.0;
/// How much wider than its [`IconSize::tile_width`] a tile may grow to fill
/// the row; past that the row keeps its leftover space on the right.
const MAX_TILE_STRETCH: f32 = 1.3;

/// Sizes of the grid's tiles for a view width, derived from the icon size
/// and the body font so larger text doesn't clip.
#[derive(Debug, Clone, Copy)]
struct GridMetrics {
    columns: usize,
    tile: Vec2,
    /// The preview area at the top of a tile. Its width follows the icon
    /// size, not the stretched tile, so previews stay one size per row.
    preview: Vec2,
    line_height: f32,
    /// A dimmed line under the name (size, or folder in search results).
    info_line: bool,
}

impl GridMetrics {
    fn new(ui: &Ui, icon_size: IconSize, available_width: f32) -> Self {
        let font = FontId::default();
        let line_height = ui.fonts_mut(|fonts| fonts.row_height(&font));
        let info_line = matches!(icon_size, IconSize::Large | IconSize::ExtraLarge);
        let min_width = icon_size.tile_width();
        let content_width = GRID_VIEW_PADDING
            .mul_add(-2.0, available_width)
            .max(min_width);
        let columns = ((content_width + TILE_GAP) / (min_width + TILE_GAP))
            .floor()
            .max(1.0) as usize;
        let filled =
            (columns.saturating_sub(1) as f32).mul_add(-TILE_GAP, content_width) / columns as f32;
        let tile_width = filled.clamp(min_width, min_width * MAX_TILE_STRETCH);
        let preview = Vec2::new(
            GRID_TILE_PADDING.mul_add(-2.0, min_width),
            icon_size.preview_height(),
        );
        let text_lines: f32 = if info_line { 3.0 } else { 2.0 };
        let tile_height = text_lines.mul_add(
            line_height,
            GRID_TILE_PADDING.mul_add(2.0, preview.y) + PREVIEW_NAME_GAP,
        );
        Self {
            columns,
            tile: Vec2::new(tile_width, tile_height.ceil()),
            preview,
            line_height,
            info_line,
        }
    }
}

/// How a tile is drawn this frame.
#[derive(Clone, Copy)]
struct TileState {
    selected: bool,
    popup_open: bool,
    cursor: bool,
    selection_fill: Color32,
    selection_stroke: egui::Stroke,
}

/// Scrolls the grid as little as possible to show `row`, drawn or not. Runs
/// inside `show_rows`, the only place egui applies the request; `first` is
/// the first row drawn there.
fn scroll_to_grid_row(ui: &Ui, row: usize, first: usize, pitch: f32, height: f32) {
    let content_top = (first as f32).mul_add(-pitch, ui.max_rect().top());
    let top = (row as f32).mul_add(pitch, content_top);
    let rect = egui::Rect::from_min_size(
        egui::pos2(ui.max_rect().left(), top),
        Vec2::new(1.0, height),
    );
    ui.scroll_to_rect_animation(rect, None, egui::style::ScrollAnimation::none());
}

impl MyTabViewer<'_> {
    // Keep the virtualized tile layout together, as in the list view.
    #[allow(clippy::too_many_lines)]
    pub(super) fn grid_view(&mut self, ui: &mut Ui, tab: &mut TabData) {
        super::ensure_text_cache_style(ui);
        let metrics = GridMetrics::new(ui, self.assets.icon_size(), ui.available_width());
        let columns = metrics.columns;
        // Keys first, so the moved cursor is drawn and scrolled to this frame.
        self.handle_keyboard_navigation(ui, tab, columns, tab.view.page_rows);
        let Some(header) = Self::begin_view(ui, tab) else {
            return;
        };
        self.show_full_paths = tab.is_searching() || tab.deep_or_multiple_paths();
        let entries_len = tab.visible_entries.len();
        let row_count = entries_len.div_ceil(columns);
        // `show_rows` puts each row `item_spacing.y` below the previous one.
        let row_pitch = metrics.tile.y + TILE_GAP;
        let ring_row = self.cursor_ring_row(tab);
        let selection_fill = self.selection_fill(ui, tab);
        let selection_stroke = {
            let stroke = ui.visuals().selection.stroke;
            egui::Stroke::new(stroke.width, ui.visuals().gray_out(stroke.color))
        };
        let selection_stroke = if selection_fill == ui.visuals().selection.bg_fill {
            ui.visuals().selection.stroke
        } else {
            selection_stroke
        };
        let scope_root = tab
            .current_path
            .single_path()
            .map(|root| root.to_string_lossy().into_owned());

        let mut row_results: Vec<RowResult> = Vec::with_capacity(entries_len.min(64));
        let inspector_owns_preview = ui
            .ctx()
            .data(|data| data.get_temp::<bool>(Id::unique("inspector_owns_preview")))
            .unwrap_or(false);
        let mut selected_inline_budget: u32 =
            u32::from(self.assets.selected_previews_enabled() && !inspector_owns_preview);

        let mut area = egui::ScrollArea::vertical().auto_shrink([false, false]);
        let mut cursor_row = None;
        match header.scroll {
            Some(ScrollRequest::Offset(offset)) => area = area.vertical_scroll_offset(offset),
            Some(ScrollRequest::Cursor(cursor)) => cursor_row = Some(cursor / columns),
            None => {}
        }
        let mut grid_left = None;
        let output = ui
            .scope(|ui| {
                ui.spacing_mut().item_spacing = Vec2::splat(TILE_GAP);
                area.show_rows(ui, metrics.tile.y, row_count, |ui, row_range| {
                    let visible_rows = row_range.clone();
                    if let Some(row) = cursor_row {
                        scroll_to_grid_row(ui, row, row_range.start, row_pitch, metrics.tile.y);
                    }
                    for row in row_range {
                        ui.horizontal_top(|ui| {
                            ui.add_space(GRID_VIEW_PADDING);
                            grid_left.get_or_insert_with(|| ui.cursor().left());

                            for col in 0..columns {
                                let row_index = row * columns + col;
                                if row_index >= entries_len {
                                    break;
                                }

                                let Some(val) = tab.entry_at(row_index) else {
                                    continue;
                                };
                                let is_cut = self.is_cut(&val);
                                let state = TileState {
                                    selected: tab.selection.contains(row_index),
                                    popup_open: row_index == header.opened_popup,
                                    cursor: ring_row == Some(row_index),
                                    selection_fill,
                                    selection_stroke,
                                };
                                let mut draw = |ui: &mut Ui| {
                                    self.draw_grid_tile(
                                        ui,
                                        &val,
                                        &metrics,
                                        state,
                                        scope_root.as_deref(),
                                        &mut selected_inline_budget,
                                    )
                                };
                                let response = if is_cut {
                                    ui.scope(|ui| {
                                        ui.multiply_opacity(0.5);
                                        draw(ui)
                                    })
                                    .inner
                                } else {
                                    draw(ui)
                                };
                                row_results.push(RowResult {
                                    row_index,
                                    response,
                                });
                            }
                        });
                    }
                    self.prefetch_grid_rows(ui.ctx(), tab, visible_rows, columns, metrics.preview);
                })
            })
            .inner;
        tab.view.scroll_y = output.state.clamped_offset().y;
        tab.view.row_pitch = row_pitch;
        tab.view.row_gap = TILE_GAP;
        tab.view.viewport_height = output.inner_rect.height();
        tab.view.rows_area = Some(output.inner_rect);
        tab.view.columns = columns;
        tab.view.tiles = Some(GridGeometry {
            left: grid_left.unwrap_or_else(|| output.inner_rect.left() + GRID_VIEW_PADDING),
            tile: metrics.tile,
            gap: Vec2::splat(TILE_GAP),
        });
        // Full rows that fit, less one kept for context.
        tab.view.page_rows = ((output.inner_rect.height() / row_pitch).floor() as usize)
            .saturating_sub(1)
            .max(1);

        self.process_row_interactions(ui, tab, &header, &row_results);
        self.quick_look_ui(ui.ctx(), tab);
    }
    fn prefetch_grid_rows(
        &mut self,
        ctx: &egui::Context,
        tab: &TabData,
        visible_rows: std::ops::Range<usize>,
        columns: usize,
        preview_size: Vec2,
    ) {
        let row_count = tab.visible_entries.len().div_ceil(columns);
        let pixels_per_point = ctx.pixels_per_point();
        let overscan_rows = visible_rows.len().max(1);
        let prefetch_start = visible_rows.start.saturating_sub(overscan_rows);
        let prefetch_end = visible_rows
            .end
            .saturating_add(overscan_rows)
            .min(row_count);
        for row in prefetch_start..prefetch_end {
            if visible_rows.contains(&row) {
                continue;
            }
            for col in 0..columns {
                let index = row * columns + col;
                if let Some(entry) = tab.entry_at(index) {
                    self.assets.prefetch_entry_texture_for_size(
                        &entry,
                        preview_size,
                        pixels_per_point,
                    );
                }
            }
        }
    }

    fn draw_grid_tile(
        &mut self,
        ui: &mut Ui,
        entry: &EntryRef<'_>,
        metrics: &GridMetrics,
        state: TileState,
        scope_root: Option<&str>,
        selected_inline_budget: &mut u32,
    ) -> egui::Response {
        let (rect, response) = ui.allocate_exact_size(metrics.tile, Sense::click_and_drag());
        let visuals = ui.visuals();
        let hovered = response.hovered() || state.popup_open;
        let bg_fill = if state.selected {
            state.selection_fill
        } else if hovered {
            visuals.widgets.hovered.bg_fill.gamma_multiply(0.45)
        } else {
            Color32::TRANSPARENT
        };
        let text_color = visuals.text_color();
        let weak_color = visuals.weak_text_color();
        let border = crate::app::ui::Palette::of(ui).border;

        ui.painter().rect_filled(rect, 8.0, bg_fill);
        if state.selected {
            ui.painter()
                .rect_stroke(rect, 8.0, state.selection_stroke, egui::StrokeKind::Inside);
        }
        if state.cursor {
            draw_cursor_ring(ui, rect, 6.0);
        }

        let inner = rect.shrink(GRID_TILE_PADDING);
        let preview_box = egui::Rect::from_center_size(
            egui::pos2(inner.center().x, inner.top() + metrics.preview.y / 2.0),
            metrics.preview,
        );
        let preview_intent = tile_preview_intent(
            entry_has_animated_preview(entry),
            response.hovered()
                && !(state.selected
                    && ui
                        .ctx()
                        .data(|data| data.get_temp::<bool>(Id::unique("inspector_owns_preview")))
                        .unwrap_or(false)),
            state.selected,
            selected_inline_budget,
        );
        self.draw_grid_preview(ui, entry, preview_intent, preview_box, border);

        let text_top = preview_box.bottom() + PREVIEW_NAME_GAP;
        let name = text_fit::fit_two_lines(
            ui,
            entry.file_name,
            &FontId::default(),
            text_color,
            inner.width(),
        );
        let name_height = name.size().y;
        if ui.is_rect_visible(rect) {
            // Centered galleys are laid out around x = 0.
            ui.painter()
                .galley(egui::pos2(inner.center().x, text_top), name, text_color);
        }
        if metrics.info_line {
            let info = self.tile_info(ui, entry, scope_root, weak_color, inner.width());
            if let Some(info) = info {
                let top = text_top + name_height.max(metrics.line_height);
                ui.painter().galley(
                    egui::pos2(inner.center().x - info.size().x / 2.0, top),
                    info,
                    weak_color,
                );
            }
        }

        describe_entry(&response, entry, state.selected);
        // Search results can share a name; the tooltip tells them apart.
        if self.show_full_paths {
            response.on_hover_text(entry.full_path_string())
        } else {
            response.on_hover_text(entry.file_name)
        }
    }

    /// The dimmed line under a large tile's name: where a search result is,
    /// or a file's size.
    fn tile_info(
        &self,
        ui: &Ui,
        entry: &EntryRef<'_>,
        scope_root: Option<&str>,
        color: Color32,
        width: f32,
    ) -> Option<std::sync::Arc<egui::Galley>> {
        let font = egui::TextStyle::Small.resolve(ui.style());
        if self.show_full_paths {
            let (dir, _) = entry.get_splitted_path();
            let shown = super::list_view::relative_dir(dir, scope_root);
            return (!shown.is_empty())
                .then(|| text_fit::fit_path_start(ui, &shown, &font, color, width));
        }
        if !entry.is_file() || entry.is_package() {
            return None;
        }
        let size = entry.meta.size;
        let galley = text_fit::label(
            ui,
            &("tile_size", size),
            || egui_mev_core::format::format_bytes(size),
            &font,
            color,
        );
        Some(text_fit::fit_single_line(ui, galley, width, 0))
    }

    /// Draws the preview scaled into `area` and resting on its bottom edge,
    /// so previews of different shapes line up above the names.
    fn draw_grid_preview(
        &mut self,
        ui: &Ui,
        entry: &EntryRef<'_>,
        intent: Option<PreviewIntent>,
        area: egui::Rect,
        border: Color32,
    ) {
        let animated = intent.and_then(|intent| {
            match self
                .assets
                .request_preview_at_size(ui.ctx(), entry, intent, area.size())
            {
                Preview::Ready(texture) => Some((texture, true)),
                Preview::Pending | Preview::Unavailable { .. } | Preview::Fallback => None,
            }
        });
        let texture = animated.or_else(|| {
            self.assets.request_entry_texture_for_size(
                entry,
                area.size(),
                ui.ctx().pixels_per_point(),
            )
        });
        if !ui.is_rect_visible(area) {
            return;
        }
        let Some((texture, is_thumbnail)) = texture else {
            // Not loaded yet: a quiet placeholder instead of a gap that pops.
            let side = area.width().min(area.height()) * 0.6;
            let placeholder = egui::Rect::from_min_size(
                egui::pos2(area.center().x - side / 2.0, area.bottom() - side),
                Vec2::splat(side),
            );
            ui.painter()
                .rect_filled(placeholder, 6.0, ui.visuals().faint_bg_color);
            return;
        };
        let size = texture.size_vec2();
        let scale = (area.width() / size.x).min(area.height() / size.y);
        let shown = size * scale;
        let rect = egui::Rect::from_min_size(
            egui::pos2(area.center().x - shown.x / 2.0, area.bottom() - shown.y),
            shown,
        );
        if is_thumbnail {
            // Pictures get a frame so light ones don't melt into the background.
            egui::Image::new(&texture)
                .corner_radius(4.0)
                .paint_at(ui, rect);
            ui.painter().rect_stroke(
                rect,
                4.0,
                egui::Stroke::new(1.0, border),
                egui::StrokeKind::Outside,
            );
        } else {
            egui::Image::new(&texture).paint_at(ui, rect);
        }
    }
}
