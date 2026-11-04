//! `dirfleet.app.show_table`: a list of a plugin's results, sorted by clicking
//! a column's header, drawn in the action's dialog (`panel`). Like forms, it is
//! data: no Lua runs while it is open.

use crate::app::ui::Palette;
use egui::{Align2, Color32, FontId, RichText, Sense, Ui};
use serde::Deserialize;
use std::cmp::Ordering;
use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::time::Instant;

const VALUE_COLUMN_WIDTH: f32 = 104.0;
/// Room at the right of the header and rows for the scrollbar, which floats
/// over them otherwise, covering the last column.
const SCROLL_GUTTER: f32 = 14.0;
/// The size filter's number and unit together, to line it up on the right.
const FILTER_CONTROLS_WIDTH: f32 = 104.0;
/// The spinner or warning before a row's status, with its gaps.
const STATUS_MARK_WIDTH: f32 = 18.0;
/// Stands in for an empty cell, as in the file list.
const NO_VALUE: &str = "—";

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ColumnKind {
    #[default]
    Text,
    /// Bytes.
    Size,
    /// Seconds since 1970.
    Time,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ColumnDef {
    pub id: String,
    pub label: String,
    #[serde(default)]
    pub kind: ColumnKind,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(untagged)]
pub enum Cell {
    Number(f64),
    Text(String),
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RowDef {
    /// Double-clicking the row opens this folder in a new tab.
    pub path: Option<PathBuf>,
    /// Cells by column id; a missing one shows as blank and sorts last.
    pub cells: BTreeMap<String, Cell>,
    /// More entries for the row's context menu.
    #[serde(default)]
    pub menu: Vec<RowAction>,
}

/// A context menu entry that starts a plugin action on the row's folder.
/// Unavailable during the table's own job, which may be reading that folder.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RowAction {
    pub label: String,
    /// The full id: `plugin.id.action`.
    pub action: String,
    /// Draw it as a warning: it removes something.
    #[serde(default)]
    pub danger: bool,
    /// What the row says while this runs, such as "Clearing target…".
    pub busy: Option<String>,
    /// The confirmation's title, shown while the plugin prepares it.
    pub title: Option<String>,
}

/// What is happening to a row's folder because of its menu.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RowStatus {
    /// Queued behind another job.
    Waiting,
    /// The action is running, with what the entry said it does.
    Working(String),
    /// Done, and new sizes are on their way.
    Measuring,
    /// It failed, for this reason. The sizes are unchanged.
    Failed(String),
}

impl RowStatus {
    const fn is_busy(&self) -> bool {
        !matches!(self, Self::Failed(_))
    }

    fn text(&self) -> &str {
        match self {
            Self::Waiting => "Waiting…",
            Self::Working(text) => text,
            Self::Measuring => "Measuring…",
            Self::Failed(_) => "Failed",
        }
    }
}

/// What `update_table_row` passes: a change to the row showing `path`.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RowUpdate {
    pub path: PathBuf,
    /// Cells to set; others keep their values.
    #[serde(default)]
    pub cells: BTreeMap<String, Cell>,
    /// Cells to blank.
    #[serde(default)]
    pub clear: Vec<String>,
    /// The folder is gone: drop the row.
    #[serde(default)]
    pub remove: bool,
}

/// What was asked for on a row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RowClick {
    Open(PathBuf),
    MeasureAgain(PathBuf),
    Action { path: PathBuf, item: RowAction },
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SortDef {
    pub column: String,
    #[serde(default)]
    pub descending: bool,
}

/// What a plugin passes to `show_table`.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TableSpec {
    pub title: String,
    pub subtitle: Option<String>,
    /// What is still happening, such as "Scanning 12 of 136". Leave it out of
    /// the last update.
    pub progress: Option<String>,
    /// An action of this plugin that measures one folder again, run with the
    /// row's folder after a menu entry changed it.
    pub refresh_action: Option<String>,
    pub columns: Vec<ColumnDef>,
    #[serde(default)]
    pub rows: Vec<RowDef>,
    pub sort: Option<SortDef>,
}

impl TableSpec {
    pub fn check(&self) -> Result<(), String> {
        if self.columns.is_empty() {
            return Err("show_table needs at least one column".into());
        }
        if let Some(sort) = &self.sort
            && !self.columns.iter().any(|column| column.id == sort.column)
        {
            return Err(format!("show_table: no column \"{}\"", sort.column));
        }
        Ok(())
    }
}

/// Units for the smallest size to show. They match how sizes are displayed,
/// where 1 GB is 1024 MB, so what is typed compares with what is read.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum SizeUnit {
    Mb,
    #[default]
    Gb,
}

impl SizeUnit {
    const fn bytes(self) -> f64 {
        match self {
            Self::Mb => 1024.0 * 1024.0,
            Self::Gb => 1024.0 * 1024.0 * 1024.0,
        }
    }

    const fn label(self) -> &'static str {
        match self {
            Self::Mb => "MB",
            Self::Gb => "GB",
        }
    }
}

/// Hides rows smaller than this. Zero shows everything.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct SizeFilter {
    pub amount: f64,
    pub unit: SizeUnit,
}

impl SizeFilter {
    /// A row not measured yet stays: it isn't known to be small.
    fn keeps(self, cell: Option<&Cell>) -> bool {
        match cell {
            Some(Cell::Number(bytes)) => *bytes >= self.amount * self.unit.bytes(),
            _ => true,
        }
    }
}

/// A column, and whether it runs from the largest down.
pub type SortOrder = (usize, bool);

/// A plugin's table and how it is sorted now.
#[derive(Debug)]
pub struct TableView {
    spec: TableSpec,
    /// Row indices in display order.
    order: Vec<usize>,
    sorted_by: Option<SortOrder>,
    filter: SizeFilter,
    /// What is happening to rows, by folder, until new sizes arrive.
    statuses: HashMap<PathBuf, RowStatus>,
    updated: Instant,
    /// Whether the plugin's job is still running. When it stops short of a
    /// last update, `progress` no longer describes anything.
    running: bool,
}

fn compare(a: Option<&Cell>, b: Option<&Cell>, descending: bool) -> Ordering {
    let direct = |ordering: Ordering| {
        if descending {
            ordering.reverse()
        } else {
            ordering
        }
    };
    match (a, b) {
        (None, None) => Ordering::Equal,
        // Blank cells stay at the end in both directions.
        (None, Some(_)) => Ordering::Greater,
        (Some(_), None) => Ordering::Less,
        (Some(Cell::Number(a)), Some(Cell::Number(b))) => direct(a.total_cmp(b)),
        (Some(Cell::Text(a)), Some(Cell::Text(b))) => {
            direct(a.to_lowercase().cmp(&b.to_lowercase()))
        }
        (Some(Cell::Number(_)), Some(Cell::Text(_))) => direct(Ordering::Less),
        (Some(Cell::Text(_)), Some(Cell::Number(_))) => direct(Ordering::Greater),
    }
}

fn cell_text(kind: ColumnKind, cell: Option<&Cell>) -> String {
    match (kind, cell) {
        (_, None) => String::new(),
        (_, Some(Cell::Text(text))) => text.clone(),
        (ColumnKind::Size, Some(Cell::Number(bytes))) => {
            crate::helper::format_bytes_simple(bytes.max(0.0) as u64)
        }
        (ColumnKind::Time, Some(Cell::Number(seconds))) => {
            chrono::DateTime::from_timestamp(*seconds as i64, 0)
                .map_or_else(String::new, |time| time.format("%Y-%m-%d").to_string())
        }
        (ColumnKind::Text, Some(Cell::Number(number))) => number.to_string(),
    }
}

impl TableView {
    pub fn new(spec: TableSpec) -> Self {
        let mut view = Self {
            order: (0..spec.rows.len()).collect(),
            sorted_by: None,
            filter: SizeFilter::default(),
            statuses: HashMap::new(),
            updated: Instant::now(),
            running: true,
            spec,
        };
        if let Some(sort) = view.spec.sort.clone()
            && let Some(column) = view.spec.columns.iter().position(|c| c.id == sort.column)
        {
            view.sort(column, sort.descending);
        }
        view
    }

    pub const fn sorted_by(&self) -> Option<SortOrder> {
        self.sorted_by
    }

    /// Sorts the way an earlier table was, if it has that column.
    pub fn apply_sort(&mut self, (column, descending): SortOrder) {
        if column < self.spec.columns.len() {
            self.sort(column, descending);
        }
    }

    pub const fn filter(&self) -> SizeFilter {
        self.filter
    }

    pub const fn set_filter(&mut self, filter: SizeFilter) {
        self.filter = filter;
    }

    /// The rows to show, in display order. The filter applies to the first
    /// size column.
    fn visible(&self) -> Vec<usize> {
        let column = self
            .spec
            .columns
            .iter()
            .find(|column| column.kind == ColumnKind::Size);
        self.order
            .iter()
            .copied()
            .filter(|&row| {
                column.is_none_or(|column| {
                    self.filter.keeps(self.spec.rows[row].cells.get(&column.id))
                })
            })
            .collect()
    }

    /// Sets or clears what a row shows beside its name.
    pub fn set_status(&mut self, path: &Path, status: Option<RowStatus>) {
        match status {
            Some(status) => self.statuses.insert(path.to_path_buf(), status),
            None => self.statuses.remove(path),
        };
    }

    pub fn status(&self, path: &Path) -> Option<&RowStatus> {
        self.statuses.get(path)
    }

    /// The control for the smallest size to show, flush with the row's right
    /// edge, if there are sizes at all.
    pub fn filter_ui(&mut self, ui: &mut Ui) {
        if !self
            .spec
            .columns
            .iter()
            .any(|column| column.kind == ColumnKind::Size)
        {
            return;
        }
        let text = "Hide smaller than";
        let font = egui::TextStyle::Body.resolve(ui.style());
        let color = ui.visuals().text_color();
        let label_width =
            ui.fonts_mut(|fonts| fonts.layout_no_wrap(text.to_owned(), font, color).size().x);
        let spacing = ui.spacing().item_spacing.x;
        let width = spacing.mul_add(2.0, label_width + FILTER_CONTROLS_WIDTH);
        ui.add_space((ui.available_width() - width).max(spacing));
        let label = ui.label(text);
        ui.add(
            egui::DragValue::new(&mut self.filter.amount)
                .range(0.0..=1_000_000.0)
                .speed(1.0)
                .max_decimals(2),
        )
        .labelled_by(label.id);
        egui::ComboBox::from_id_salt("plugin_table_size_unit")
            .selected_text(self.filter.unit.label())
            .width(64.0)
            .show_ui(ui, |ui| {
                for unit in [SizeUnit::Mb, SizeUnit::Gb] {
                    ui.selectable_value(&mut self.filter.unit, unit, unit.label());
                }
            });
    }

    pub fn refresh_action(&self) -> Option<&str> {
        self.spec.refresh_action.as_deref()
    }

    pub fn has_row(&self, path: &Path) -> bool {
        self.spec
            .rows
            .iter()
            .any(|row| row.path.as_deref() == Some(path))
    }

    /// Changes one row, so its sizes needn't wait for a whole new scan.
    pub fn update_row(&mut self, update: RowUpdate) {
        let Some(index) = self
            .spec
            .rows
            .iter()
            .position(|row| row.path.as_ref() == Some(&update.path))
        else {
            return;
        };
        self.statuses.remove(&update.path);
        if update.remove {
            self.spec.rows.remove(index);
            self.order.retain(|&row| row != index);
            for row in &mut self.order {
                if *row > index {
                    *row -= 1;
                }
            }
        } else {
            let row = &mut self.spec.rows[index];
            row.cells.extend(update.cells);
            for cell in &update.clear {
                row.cells.remove(cell);
            }
        }
        if let Some((column, descending)) = self.sorted_by {
            self.sort(column, descending);
        }
    }

    /// The subtitle with `{count}` and `{total}` filled in from the rows now
    /// in the table, so they stay right as rows change.
    #[allow(clippy::literal_string_with_formatting_args)]
    fn subtitle_text(&self) -> Option<String> {
        let subtitle = self.spec.subtitle.as_ref()?;
        if !subtitle.contains('{') {
            return Some(subtitle.clone());
        }
        let size = self
            .spec
            .columns
            .iter()
            .find(|column| column.kind == ColumnKind::Size);
        let total: f64 = size.map_or(0.0, |column| {
            self.spec
                .rows
                .iter()
                .filter_map(|row| match row.cells.get(&column.id) {
                    Some(Cell::Number(bytes)) => Some(*bytes),
                    _ => None,
                })
                .sum()
        });
        Some(
            subtitle
                .replace("{count}", &self.spec.rows.len().to_string())
                .replace(
                    "{total}",
                    &crate::helper::format_bytes_simple(total.max(0.0) as u64),
                ),
        )
    }

    /// The context menu of a row with a folder.
    fn row_menu_ui(&self, ui: &mut Ui, row: &RowDef, path: &Path) -> Option<RowClick> {
        let mut click = None;
        if ui.button("Open in new tab").clicked() {
            click = Some(RowClick::Open(path.to_path_buf()));
            ui.close();
        }
        if ui.button("Copy path").clicked() {
            ui.ctx().copy_text(path.display().to_string());
            ui.close();
        }
        if self.spec.refresh_action.is_some() {
            let again = ui
                .add_enabled(!self.running, egui::Button::new("Measure again"))
                .on_disabled_hover_text("Stop the scan first");
            if again.clicked() {
                click = Some(RowClick::MeasureAgain(path.to_path_buf()));
                ui.close();
            }
        }
        if !row.menu.is_empty() {
            ui.separator();
        }
        let busy = self.status(path).is_some_and(RowStatus::is_busy);
        let enabled = !self.running && !busy;
        let reason = if busy {
            "Wait for this folder to finish"
        } else {
            "Stop the scan first"
        };
        for item in &row.menu {
            let mut text = RichText::new(&item.label);
            if item.danger && enabled {
                text = text.color(ui.visuals().error_fg_color);
            }
            let button = ui
                .add_enabled(enabled, egui::Button::new(text))
                .on_disabled_hover_text(reason);
            if button.clicked() {
                click = Some(RowClick::Action {
                    path: path.to_path_buf(),
                    item: item.clone(),
                });
                ui.close();
            }
        }
        click
    }

    /// What is still happening, while the job runs.
    pub fn progress(&self) -> Option<&str> {
        self.spec.progress.as_deref()
    }

    pub const fn set_running(&mut self, running: bool) {
        self.running = running;
    }

    /// New contents for the open table. The order is kept, so ties stay as
    /// the user's last sort left them.
    pub fn update(&mut self, spec: TableSpec) {
        if spec.rows.len() != self.spec.rows.len() {
            self.order = (0..spec.rows.len()).collect();
        }
        self.spec = spec;
        self.updated = Instant::now();
        self.running = true;
        if let Some((column, descending)) = self.sorted_by
            && column < self.spec.columns.len()
        {
            self.sort(column, descending);
        }
    }

    /// Stable, so rows that tie stay in the order of the previous sort:
    /// clicking "Last change" and then "Size" means size, ties by date.
    fn sort(&mut self, column: usize, descending: bool) {
        let id = &self.spec.columns[column].id;
        let rows = &self.spec.rows;
        self.order
            .sort_by(|&a, &b| compare(rows[a].cells.get(id), rows[b].cells.get(id), descending));
        self.sorted_by = Some((column, descending));
    }

    /// A header was clicked: numbers start from the largest, and a second
    /// click reverses.
    fn header_clicked(&mut self, column: usize) {
        let descending = match self.sorted_by {
            Some((current, descending)) if current == column => !descending,
            _ => self.spec.columns[column].kind != ColumnKind::Text,
        };
        self.sort(column, descending);
    }

    fn updated_text(&self) -> String {
        let minutes = self.updated.elapsed().as_secs() / 60;
        match minutes {
            0 => "just now".to_owned(),
            1..60 => format!("{minutes} min ago"),
            _ => format!("{} h ago", minutes / 60),
        }
    }

    /// The column rectangles of a row: the first takes what the others leave.
    fn column_rects(&self, row: egui::Rect) -> Vec<egui::Rect> {
        let others = self.spec.columns.len().saturating_sub(1) as f32;
        let width = row.width() - SCROLL_GUTTER;
        let first = others.mul_add(-VALUE_COLUMN_WIDTH, width).max(80.0);
        let mut left = row.left();
        (0..self.spec.columns.len())
            .map(|index| {
                let width = if index == 0 {
                    first
                } else {
                    VALUE_COLUMN_WIDTH
                };
                let rect = egui::Rect::from_min_size(
                    egui::pos2(left, row.top()),
                    egui::vec2(width, row.height()),
                );
                left += width;
                rect.shrink2(egui::vec2(8.0, 0.0))
            })
            .collect()
    }

    fn paint_cell(
        ui: &Ui,
        rect: egui::Rect,
        first: bool,
        text: &str,
        font: FontId,
        color: Color32,
    ) {
        let (anchor, position) = if first {
            (Align2::LEFT_CENTER, rect.left_center())
        } else {
            (Align2::RIGHT_CENTER, rect.right_center())
        };
        ui.painter()
            .with_clip_rect(rect.intersect(ui.clip_rect()))
            .text(position, anchor, text, font, color);
    }

    fn header_ui(&mut self, ui: &mut Ui) {
        let (row, _) = ui.allocate_exact_size(
            egui::vec2(ui.available_width(), row_height(ui)),
            Sense::hover(),
        );
        let rects = self.column_rects(row);
        let mut clicked = None;
        for (index, rect) in rects.iter().enumerate() {
            let column = &self.spec.columns[index];
            let arrow = match self.sorted_by {
                Some((current, true)) if current == index => Some(lucide_icons::Icon::ArrowDown),
                Some((current, false)) if current == index => Some(lucide_icons::Icon::ArrowUp),
                _ => None,
            };
            let response = ui.interact(
                rect.expand2(egui::vec2(8.0, 0.0)),
                ui.id().with(("table_header", index)),
                Sense::click(),
            );
            if response.clicked() {
                clicked = Some(index);
            }
            response.widget_info(|| {
                egui::WidgetInfo::labeled(egui::WidgetType::Button, true, &column.label)
            });
            let color = if response.hovered() {
                ui.visuals().strong_text_color()
            } else {
                Palette::of(ui).secondary
            };
            let mut job = egui::text::LayoutJob::default();
            job.append(
                &column.label,
                0.0,
                egui::TextFormat::simple(egui::TextStyle::Body.resolve(ui.style()), color),
            );
            let lucide = egui::FontFamily::Name("lucide".into());
            if let Some(arrow) = arrow
                && ui.fonts_mut(|fonts| fonts.families().contains(&lucide))
            {
                job.append(
                    &char::from(arrow).to_string(),
                    4.0,
                    egui::TextFormat::simple(FontId::new(12.0, lucide), color),
                );
            }
            let galley = ui.fonts_mut(|fonts| fonts.layout_job(job));
            let left = if index == 0 {
                rect.left()
            } else {
                rect.right() - galley.size().x
            };
            let top = rect.center().y - galley.size().y / 2.0;
            ui.painter().galley(egui::pos2(left, top), galley, color);
        }
        if let Some(index) = clicked {
            self.header_clicked(index);
        }
    }

    /// A row's first cell: its name, then what is happening to its folder.
    fn paint_name(ui: &Ui, rect: egui::Rect, name: &str, status: Option<&RowStatus>, dim: f32) {
        let font = egui::TextStyle::Body.resolve(ui.style());
        let color = ui.visuals().text_color().gamma_multiply(dim);
        let painter = ui.painter().with_clip_rect(rect.intersect(ui.clip_rect()));
        let Some(status) = status else {
            painter.text(rect.left_center(), Align2::LEFT_CENTER, name, font, color);
            return;
        };
        let small = egui::TextStyle::Small.resolve(ui.style());
        let failed = matches!(status, RowStatus::Failed(_));
        let status_color = if failed {
            ui.visuals().error_fg_color
        } else {
            Palette::of(ui).secondary
        };
        let status_galley = ui
            .fonts_mut(|fonts| fonts.layout_no_wrap(status.text().to_owned(), small, status_color));
        let status_width = status_galley.size().x + STATUS_MARK_WIDTH;
        // The name gives way first, keeping the status readable.
        let name_rect = egui::Rect::from_min_max(
            rect.min,
            egui::pos2(
                (rect.right() - status_width - 8.0).max(rect.left()),
                rect.bottom(),
            ),
        );
        let name_galley = ui.fonts_mut(|fonts| fonts.layout_no_wrap(name.to_owned(), font, color));
        let name_width = name_galley.size().x.min(name_rect.width());
        ui.painter()
            .with_clip_rect(name_rect.intersect(ui.clip_rect()))
            .galley(
                egui::pos2(rect.left(), rect.center().y - name_galley.size().y / 2.0),
                name_galley,
                color,
            );
        let mark = egui::Rect::from_center_size(
            egui::pos2(rect.left() + name_width + 8.0 + 6.0, rect.center().y),
            egui::vec2(12.0, 12.0),
        );
        if failed {
            painter.text(
                mark.center(),
                Align2::CENTER_CENTER,
                char::from(lucide_icons::Icon::TriangleAlert).to_string(),
                FontId::new(12.0, egui::FontFamily::Name("lucide".into())),
                status_color,
            );
        } else {
            egui::Spinner::new()
                .size(12.0)
                .color(status_color)
                .paint_at(ui, mark);
        }
        painter.galley(
            egui::pos2(
                mark.right() + 6.0,
                rect.center().y - status_galley.size().y / 2.0,
            ),
            status_galley,
            status_color,
        );
    }

    fn rows_ui(&self, ui: &mut Ui, visible: &[usize]) -> Option<RowClick> {
        let mut open = None;
        let height = row_height(ui);
        egui::ScrollArea::vertical()
            .auto_shrink([false, false])
            .show_rows(ui, height, visible.len(), |ui, range| {
                ui.spacing_mut().item_spacing.y = 0.0;
                for &index in &visible[range] {
                    let row = &self.spec.rows[index];
                    let status = row.path.as_deref().and_then(|path| self.status(path));
                    let (rect, response) = ui.allocate_exact_size(
                        egui::vec2(ui.available_width(), height),
                        Sense::click(),
                    );
                    // The cells are painted, so screen readers need the row's text.
                    response.widget_info(|| {
                        let mut text: Vec<String> = self
                            .spec
                            .columns
                            .iter()
                            .map(|column| cell_text(column.kind, row.cells.get(&column.id)))
                            .filter(|text| !text.is_empty())
                            .collect();
                        if let Some(status) = status {
                            text.push(status.text().to_owned());
                        }
                        egui::WidgetInfo::labeled(egui::WidgetType::Button, true, text.join(", "))
                    });
                    if response.hovered() {
                        ui.painter().rect_filled(
                            rect,
                            crate::app::ui::control_radius(ui),
                            ui.visuals().widgets.hovered.weak_bg_fill,
                        );
                    }
                    let dim = if status.is_some_and(RowStatus::is_busy) {
                        0.6
                    } else {
                        1.0
                    };
                    for (position, (cell_rect, column)) in self
                        .column_rects(rect)
                        .into_iter()
                        .zip(&self.spec.columns)
                        .enumerate()
                    {
                        let text = cell_text(column.kind, row.cells.get(&column.id));
                        if position == 0 {
                            Self::paint_name(ui, cell_rect, &text, status, dim);
                        } else {
                            let empty = text.is_empty();
                            let color = if empty {
                                ui.visuals().weak_text_color()
                            } else {
                                ui.visuals().text_color().gamma_multiply(dim)
                            };
                            Self::paint_cell(
                                ui,
                                cell_rect,
                                false,
                                if empty { NO_VALUE } else { &text },
                                egui::TextStyle::Body.resolve(ui.style()),
                                color,
                            );
                        }
                    }
                    if let Some(path) = &row.path {
                        let mut tip = path.display().to_string();
                        if let Some(RowStatus::Failed(reason)) = status {
                            tip = format!("{tip}\n{reason}");
                        }
                        let response = response.on_hover_text(tip);
                        if response.double_clicked() {
                            open = Some(RowClick::Open(path.clone()));
                        }
                        response.context_menu(|ui| {
                            if let Some(click) = self.row_menu_ui(ui, row, path) {
                                open = Some(click);
                            }
                        });
                    }
                }
            });
        open
    }

    /// The line above the table: what it adds up to on the left, how current
    /// it is on the right.
    pub fn summary_ui(&self, ui: &mut Ui) {
        let shown = self.visible().len();
        let secondary = Palette::of(ui).secondary;
        ui.horizontal(|ui| {
            if let Some(subtitle) = &self.subtitle_text() {
                ui.add(egui::Label::new(RichText::new(subtitle).color(secondary)).truncate());
            }
            if shown < self.order.len() {
                let text = format!("Showing {shown} of {}", self.order.len());
                ui.label(RichText::new(text).color(secondary));
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                match &self.spec.progress {
                    // The dialog's progress line says what is happening.
                    Some(_) if self.running => {}
                    Some(_) => {
                        ui.label(RichText::new("Stopped early").color(secondary));
                    }
                    None => {
                        let text = format!("Updated {}", self.updated_text());
                        ui.label(RichText::new(text).color(secondary));
                    }
                }
            });
        });
    }

    /// The header and rows, filling the space given, in a card.
    pub fn body_ui(&mut self, ui: &mut Ui) -> Option<RowClick> {
        let palette = Palette::of(ui);
        egui::Frame::new()
            .fill(palette.card)
            .stroke(egui::Stroke::new(1.0_f32, palette.card_border))
            .corner_radius(crate::app::ui::card_radius(ui))
            .inner_margin(egui::Margin::symmetric(4, 4))
            .show(ui, |ui| {
                ui.set_min_size(ui.available_size());
                self.header_ui(ui);
                ui.separator();
                // After the header, so that a change shows this very frame.
                let visible = self.visible();
                if visible.is_empty() {
                    ui.centered_and_justified(|ui| {
                        ui.weak(if self.order.is_empty() {
                            "Nothing to show."
                        } else {
                            "Nothing is this large."
                        });
                    });
                    return None;
                }
                self.rows_ui(ui, &visible)
            })
            .inner
    }

    /// The summary and table together, for a dialog with nothing above them.
    #[cfg(test)]
    pub fn contents_ui(&mut self, ui: &mut Ui) -> Option<RowClick> {
        self.summary_ui(ui);
        ui.add_space(8.0);
        self.body_ui(ui)
    }
}

/// The height of a row, the same as the file list's.
fn row_height(ui: &Ui) -> f32 {
    crate::app::ui::control_height(ui)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn view(rows: &serde_json::Value) -> TableView {
        let spec: TableSpec = serde_json::from_value(serde_json::json!({
            "title": "T",
            "columns": [
                { "id": "name", "label": "Name" },
                { "id": "size", "label": "Size", "kind": "size" },
                { "id": "time", "label": "Time", "kind": "time" },
            ],
            "rows": rows,
            "sort": { "column": "size", "descending": true },
        }))
        .expect("spec");
        spec.check().expect("valid");
        TableView::new(spec)
    }

    fn names(view: &TableView) -> Vec<String> {
        view.order
            .iter()
            .map(|&row| cell_text(ColumnKind::Text, view.spec.rows[row].cells.get("name")))
            .collect()
    }

    fn sample() -> TableView {
        view(&serde_json::json!([
            { "cells": { "name": "b", "size": 10, "time": 200 } },
            { "cells": { "name": "a", "size": 30, "time": 100 } },
            { "cells": { "name": "c", "size": 10, "time": 300 } },
            { "cells": { "name": "blank" } },
        ]))
    }

    #[test]
    fn sorts_and_keeps_blanks_last() {
        let mut view = sample();
        assert_eq!(names(&view), ["a", "b", "c", "blank"]);
        view.header_clicked(1);
        assert_eq!(
            names(&view),
            ["b", "c", "a", "blank"],
            "second click reverses"
        );
        view.header_clicked(0);
        assert_eq!(
            names(&view),
            ["a", "b", "blank", "c"],
            "text starts ascending"
        );
    }

    #[test]
    fn ties_keep_the_previous_order() {
        let mut view = sample();
        view.header_clicked(2);
        assert_eq!(names(&view), ["c", "b", "a", "blank"]);
        view.header_clicked(1);
        assert_eq!(names(&view), ["a", "c", "b", "blank"], "size, ties by date");
    }

    #[test]
    fn updates_keep_the_sort_and_rows_fill_in() {
        let mut table = view(&serde_json::json!([
            { "cells": { "name": "b" } },
            { "cells": { "name": "a" } },
        ]));
        table.header_clicked(1); // Ascending, from descending.
        let spec: TableSpec = serde_json::from_value(serde_json::json!({
            "title": "T",
            "columns": table.spec.columns.iter().map(|column| serde_json::json!({
                "id": column.id, "label": column.label,
                "kind": if column.id == "size" { "size" } else { "text" },
            })).collect::<Vec<_>>(),
            "rows": [
                { "cells": { "name": "b", "size": 10 } },
                { "cells": { "name": "a", "size": 5 } },
            ],
            "progress": "Scanning",
        }))
        .expect("spec");
        table.update(spec);
        assert_eq!(names(&table), ["a", "b"], "sorted the user's way");
        assert_eq!(table.sorted_by, Some((1, false)));
        assert_eq!(table.spec.progress.as_deref(), Some("Scanning"));
    }

    #[test]
    fn filter_hides_small_rows_but_not_unmeasured_ones() {
        const MB: i64 = 1024 * 1024;
        let mut table = view(&serde_json::json!([
            { "cells": { "name": "big", "size": 2048 * MB } },
            { "cells": { "name": "exact", "size": 1024 * MB } },
            { "cells": { "name": "small", "size": 1023 * MB } },
            { "cells": { "name": "unmeasured" } },
        ]));
        let shown = |table: &TableView| -> Vec<String> {
            table
                .visible()
                .iter()
                .map(|&row| cell_text(ColumnKind::Text, table.spec.rows[row].cells.get("name")))
                .collect()
        };
        assert_eq!(shown(&table).len(), 4, "off by default");
        table.set_filter(SizeFilter {
            amount: 1.0,
            unit: SizeUnit::Gb,
        });
        assert_eq!(shown(&table), ["big", "exact", "unmeasured"]);
        table.set_filter(SizeFilter {
            amount: 1023.0,
            unit: SizeUnit::Mb,
        });
        assert_eq!(shown(&table).len(), 4);
        // An update keeps it, and sorting still applies underneath.
        table.set_filter(SizeFilter {
            amount: 1.5,
            unit: SizeUnit::Gb,
        });
        table.header_clicked(1);
        assert_eq!(shown(&table), ["big", "unmeasured"]);
        table.update(table.spec.clone());
        assert_eq!(shown(&table), ["big", "unmeasured"]);
    }

    #[test]
    fn rows_update_and_go_and_the_subtitle_follows() {
        let mut table = view(&serde_json::json!([
            { "path": "/a", "cells": { "name": "a", "size": 100 }, "menu": [
                { "label": "Clear", "action": "p.clear", "danger": true }
            ] },
            { "path": "/b", "cells": { "name": "b", "size": 50 } },
            { "path": "/c", "cells": { "name": "c", "size": 10 } },
        ]));
        table.spec.subtitle = Some("Items: {count} · {total}".into());
        assert_eq!(table.subtitle_text().as_deref(), Some("Items: 3 · 160 B"));
        assert_eq!(names(&table), ["a", "b", "c"]);

        let update = |json: serde_json::Value| -> RowUpdate {
            serde_json::from_value(json).expect("update")
        };
        table.update_row(update(serde_json::json!({
            "path": "/a", "cells": { "size": 1 }, "clear": ["time"],
        })));
        assert_eq!(names(&table), ["b", "c", "a"], "sorted again");
        assert_eq!(table.spec.rows[0].menu.len(), 1, "the menu stays");
        assert_eq!(table.subtitle_text().as_deref(), Some("Items: 3 · 61 B"));

        table.update_row(update(serde_json::json!({ "path": "/b", "remove": true })));
        assert_eq!(names(&table), ["c", "a"]);
        assert!(!table.has_row(std::path::Path::new("/b")));
        assert_eq!(table.subtitle_text().as_deref(), Some("Items: 2 · 11 B"));
        table.update_row(update(
            serde_json::json!({ "path": "/gone", "remove": true }),
        ));
        assert_eq!(names(&table), ["c", "a"]);
    }

    #[test]
    fn cells_are_formatted_by_kind() {
        let bytes = Cell::Number(1_048_576.0);
        assert_eq!(
            cell_text(ColumnKind::Size, Some(&bytes)),
            crate::helper::format_bytes_simple(1_048_576)
        );
        assert_eq!(
            cell_text(ColumnKind::Time, Some(&Cell::Number(1_700_000_000.0))),
            "2023-11-14"
        );
        assert_eq!(cell_text(ColumnKind::Size, None), "");
    }

    #[test]
    fn bad_specs_are_refused() {
        let spec: TableSpec = serde_json::from_value(serde_json::json!({
            "title": "T",
            "columns": [{ "id": "a", "label": "A" }],
            "sort": { "column": "missing" },
        }))
        .expect("spec");
        assert!(spec.check().is_err());
        let spec: TableSpec = serde_json::from_value(serde_json::json!({
            "title": "T",
            "columns": [],
        }))
        .expect("spec");
        assert!(spec.check().is_err());
    }
}
