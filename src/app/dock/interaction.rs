//! Mouse and keyboard handling shared by the list and grid views, so both
//! views select, open and navigate the same way.
use super::{MyTabViewer, RowResult, TabData, ViewHeader, ViewState};
use crate::app::file_ops::{DraggedPaths, folder_drop_target};
use egui::{Id, Key, Modifiers, Pos2, Rect, Ui};
use egui_mev_core::selection::{SelectMods, Selection};
use egui_mev_widgets::select_mods;

/// How long typed characters keep extending the type-to-select prefix.
const TYPE_TO_SELECT_TIMEOUT: f64 = 1.0;

/// A drag selection in progress: where it started, in content coordinates
/// (so scrolling while dragging keeps the start in place), and what was
/// selected before it (kept when Ctrl or Shift was held).
#[derive(Clone)]
struct Marquee {
    origin: Pos2,
    base: Selection,
}

#[derive(Clone, Default)]
struct TypeToSelect {
    prefix: String,
    last_input: f64,
}

impl MyTabViewer<'_> {
    /// Applies the interactions collected while drawing the rows: selection
    /// gestures, double-click activation and the per-row context menu.
    pub(super) fn process_row_interactions(
        &self,
        ui: &Ui,
        tab: &mut TabData,
        header: &ViewHeader,
        row_results: &[RowResult],
    ) {
        let mods = select_mods(ui.input(|i| i.modifiers));
        let mut clicked: Option<(usize, SelectMods)> = None;
        let mut select_for_menu = None;
        // Recycle Bin rows show where items came from; they can't be opened,
        // dragged or dropped onto.
        let in_trash = tab.current_path.is_trash();
        for RowResult {
            row_index,
            response,
        } in row_results
        {
            let row_index = *row_index;
            let Some(entry) = tab.entry_at(row_index) else {
                continue;
            };
            if in_trash {
                if response.clicked() {
                    clicked = Some((row_index, mods));
                } else if response.secondary_clicked() && !tab.selection.contains(row_index) {
                    select_for_menu = Some(row_index);
                }
            } else if response.double_clicked() {
                // Ctrl+double-click opens a folder in a new tab.
                Self::activate_entry(&entry, tab.id, mods.command);
            } else if response.middle_clicked() && !entry.is_file() {
                Self::activate_entry(&entry, tab.id, true);
            } else if response.clicked() {
                clicked = Some((row_index, mods));
            } else if response.secondary_clicked() && !tab.selection.contains(row_index) {
                // Right-clicking outside the selection selects that item first,
                // so the menu always acts on what is highlighted.
                select_for_menu = Some(row_index);
            }
            // Dragging starts with the selection, or just this row when it
            // is not selected; folders accept drops.
            if response.drag_started() && !in_trash {
                let paths: Vec<_> = if tab.selection.contains(row_index) {
                    tab.selection
                        .rows()
                        .filter_map(|row| tab.entry_at(row).map(|e| e.get_path()))
                        .collect()
                } else {
                    select_for_menu = Some(row_index);
                    vec![entry.get_path()]
                };
                response.dnd_set_drag_payload(DraggedPaths(paths));
            }
            if !entry.is_file() && !in_trash {
                folder_drop_target(ui, response, &entry.get_path());
            }
            #[cfg(target_os = "macos")]
            if entry.is_package()
                && !in_trash
                && std::path::Path::new(entry.file_name)
                    .extension()
                    .is_some_and(|ext| ext.eq_ignore_ascii_case("app"))
            {
                crate::app::file_ops::app_drop_target(ui, response, &entry.get_path());
            }
            self.show_entry_context_menu(
                response,
                tab,
                &entry,
                row_index,
                header.tab_popup_id,
                &header.favorites,
            );
        }
        if let Some((row, mods)) = clicked {
            tab.selection.click(row, mods);
        }
        if let Some(row) = select_for_menu {
            tab.selection.select_only(row);
        }
        if header.background.clicked() && !mods.command && !mods.shift {
            tab.selection.deselect_all();
        }
        Self::drag_select(ui, tab, &header.background, mods);
        Self::show_background_menu(&header.background, tab, &header.favorites);
        // Dropping onto empty space puts items in the folder being shown.
        if let Some(folder) = tab.current_path.single_path() {
            folder_drop_target(ui, &header.background, &folder);
        }
    }

    /// Dragging across empty space selects every item the rectangle
    /// touches; with Ctrl or Shift held they are added to the selection.
    fn drag_select(ui: &Ui, tab: &mut TabData, background: &egui::Response, mods: SelectMods) {
        let id = Id::unique("drag_select").with(tab.id);
        let Some(area) = tab.view.rows_area else {
            return;
        };
        // A drag starts once the pointer has moved a little; the rectangle
        // starts where the button went down.
        if background.drag_started()
            && let Some(start) = ui.input(|input| input.pointer.press_origin())
        {
            let base = if mods.command || mods.shift {
                tab.selection.clone()
            } else {
                let mut base = tab.selection.clone();
                base.deselect_all();
                base
            };
            let marquee = Marquee {
                origin: to_content(&tab.view, area, start),
                base,
            };
            ui.data_mut(|data| data.insert_temp(id, marquee));
        }
        let Some(marquee) = ui.data(|data| data.get_temp::<Marquee>(id)) else {
            return;
        };
        if !background.dragged() {
            ui.data_mut(|data| data.remove::<Marquee>(id));
            return;
        }
        let Some(pointer) = ui.ctx().pointer_latest_pos() else {
            return;
        };
        let content = Rect::from_two_pos(marquee.origin, to_content(&tab.view, area, pointer));
        let mut selection = marquee.base;
        selection.add_rows(rows_in(&tab.view, content, tab.visible_entries.len()));
        tab.selection = selection;

        let shown = content.translate(egui::vec2(0.0, area.top() - tab.view.scroll_y));
        let accent = ui.visuals().selection.bg_fill;
        let clipped = ui.painter().with_clip_rect(area);
        clipped.rect_filled(shown, 2.0, accent.gamma_multiply(0.15));
        clipped.rect_stroke(
            shown,
            2.0,
            egui::Stroke::new(1.0, accent),
            egui::StrokeKind::Inside,
        );
    }

    /// Keyboard navigation for the focused view. `columns` is 1 for the list;
    /// `page_rows` is how many full rows fit on screen. Runs before the rows
    /// are drawn, so a key press is shown (and scrolled to) in the same frame.
    pub(super) fn handle_keyboard_navigation(
        &self,
        ui: &Ui,
        tab: &mut TabData,
        columns: usize,
        page_rows: usize,
    ) {
        if !self.focused || self.active_tab != tab.id || ui.ctx().text_edit_focused() {
            return;
        }
        let len = tab.visible_entries.len();
        if len == 0 {
            return;
        }
        let last = len - 1;
        let columns = columns.max(1);
        let page = page_rows.max(1) * columns;
        let (modifiers, key) = ui.input(|i| {
            let key = [
                Key::ArrowDown,
                Key::ArrowUp,
                Key::ArrowLeft,
                Key::ArrowRight,
                Key::Home,
                Key::End,
                Key::PageUp,
                Key::PageDown,
                Key::Enter,
                Key::Space,
                Key::A,
                Key::I,
                Key::Escape,
            ]
            .into_iter()
            .find(|key| i.key_pressed(*key));
            (i.modifiers, key)
        });

        if let Some(key) = key {
            let mods = select_mods(modifiers);
            let current = tab.selection.cursor().filter(|&row| row < len);
            let target = |from: usize| -> Option<usize> {
                Some(match key {
                    Key::ArrowDown => from.saturating_add(columns).min(last),
                    Key::ArrowUp => from.saturating_sub(columns),
                    Key::ArrowLeft if columns > 1 => from.saturating_sub(1),
                    Key::ArrowRight if columns > 1 => from.saturating_add(1).min(last),
                    Key::Home => 0,
                    Key::End => last,
                    Key::PageDown => from.saturating_add(page).min(last),
                    Key::PageUp => from.saturating_sub(page),
                    _ => return None,
                })
            };
            // In Quick Look, ←/→ also step through a one-column list.
            let key = match key {
                Key::ArrowLeft if tab.quick_look && columns == 1 => Key::ArrowUp,
                Key::ArrowRight if tab.quick_look && columns == 1 => Key::ArrowDown,
                other => other,
            };
            match key {
                Key::Space if modifiers.is_none() && tab.selection.cursor().is_some() => {
                    tab.quick_look = !tab.quick_look;
                }
                Key::Escape if tab.quick_look => tab.quick_look = false,
                Key::Enter if !modifiers.alt => {
                    tab.quick_look = false;
                    Self::activate_selection(tab, modifiers.command);
                }
                Key::Space if modifiers.command => tab.selection.toggle_cursor(),
                Key::A if modifiers.matches_exact(Modifiers::COMMAND) => {
                    tab.selection.select_all(len);
                }
                Key::I if modifiers.matches_exact(Modifiers::COMMAND | Modifiers::SHIFT) => {
                    tab.selection.invert(len);
                }
                Key::Escape if !tab.selection.is_empty() => tab.selection.deselect_all(),
                _ if modifiers.alt => {}
                _ => {
                    // With no cursor yet, the first navigation key lands on the
                    // first row (or the last one for End) instead of moving.
                    let row = match current {
                        Some(from) => target(from),
                        None if key == Key::End => Some(last),
                        None => target(0).map(|_| 0),
                    };
                    if let Some(row) = row {
                        tab.selection.move_cursor(row, mods);
                    }
                }
            }
            return;
        }
        self.type_to_select(ui, tab);
    }

    /// Jumps to the next entry whose name starts with the typed prefix.
    fn type_to_select(&self, ui: &Ui, tab: &mut TabData) {
        let (typed, now) = ui.input(|i| {
            let typed: String = if i.modifiers.command || i.modifiers.alt {
                String::new()
            } else {
                i.events
                    .iter()
                    .filter_map(|event| match event {
                        egui::Event::Text(text) => Some(text.as_str()),
                        _ => None,
                    })
                    .collect()
            };
            (typed, i.time)
        });
        if typed.is_empty() {
            return;
        }
        let id = Id::unique("type_to_select").with(self.active_tab);
        let mut state: TypeToSelect = ui.data(|d| d.get_temp(id)).unwrap_or_default();
        let continuing = now - state.last_input < TYPE_TO_SELECT_TIMEOUT;
        if !continuing {
            state.prefix.clear();
        }
        {
            use unicode_normalization::UnicodeNormalization;
            // Folded like `Listing::folded_names`.
            state
                .prefix
                .extend(typed.nfc().flat_map(char::to_lowercase));
        }
        state.last_input = now;
        // Repeating the same character cycles through matches, as in Explorer.
        let repeated_char = state.prefix.chars().count() > 1 && {
            let mut chars = state.prefix.chars();
            let first = chars.next();
            chars.all(|c| Some(c) == first)
        };
        let needle: String = if repeated_char {
            state.prefix.chars().take(1).collect()
        } else {
            state.prefix.clone()
        };
        let len = tab.visible_entries.len();
        let start = match tab.selection.cursor() {
            Some(row) if !continuing || repeated_char => row + 1,
            Some(row) => row,
            None => 0,
        };
        let folded = tab.listing.folded_names();
        let found = (0..len).map(|offset| (start + offset) % len).find(|&row| {
            tab.visible_entries
                .get(row)
                .is_some_and(|&index| folded.get(index as usize).starts_with(&needle))
        });
        if let Some(row) = found {
            tab.selection.select_only(row);
        }
        ui.data_mut(|d| d.insert_temp(id, state));
    }

    /// Opens every selected entry: files with their default application, a
    /// single folder in this tab, several folders (or `new_tab`) in new tabs.
    pub(super) fn activate_selection(tab: &TabData, new_tab: bool) {
        if tab.current_path.is_trash() {
            return;
        }
        let entries: Vec<_> = tab
            .selection
            .rows()
            .filter_map(|row| tab.entry_at(row))
            .collect();
        let folders = entries.iter().filter(|entry| !entry.is_file()).count();
        let folders_in_new_tabs = new_tab || folders > 1;
        for entry in &entries {
            Self::activate_entry(entry, tab.id, folders_in_new_tabs);
        }
    }
}

/// `pos` on screen as a point in the scrolled content: y counts from the top
/// of the first row.
fn to_content(view: &ViewState, area: Rect, pos: Pos2) -> Pos2 {
    egui::pos2(pos.x, pos.y - area.top() + view.scroll_y)
}

/// Rows (items) that `rect`, in content coordinates, touches.
fn rows_in(view: &ViewState, rect: Rect, len: usize) -> Vec<usize> {
    if view.row_pitch <= 0.0 || len == 0 || rect.max.y < 0.0 {
        return Vec::new();
    }
    let item_height = view.row_pitch - view.row_gap;
    let first_row = (rect.min.y.max(0.0) / view.row_pitch).floor() as usize;
    let last_row = (rect.max.y / view.row_pitch).floor() as usize;
    let columns = view.columns.max(1);
    let mut rows = Vec::new();
    for row in first_row..=last_row {
        // Only the gap below this row's items was touched.
        if rect.min.y > (row as f32).mul_add(view.row_pitch, item_height) {
            continue;
        }
        for column in 0..columns {
            let index = row * columns + column;
            if index >= len {
                return rows;
            }
            let touches = view.tiles.is_none_or(|tiles| {
                let left = (column as f32).mul_add(tiles.tile.x + tiles.gap.x, tiles.left);
                rect.max.x >= left && rect.min.x <= left + tiles.tile.x
            });
            if touches {
                rows.push(index);
            }
        }
    }
    rows
}

#[cfg(test)]
mod tests {
    use super::{ViewState, rows_in};
    use crate::app::dock::GridGeometry;
    use egui::{Rect, pos2, vec2};

    #[test]
    fn drag_rectangles_select_the_rows_they_touch() {
        let list = ViewState {
            row_pitch: 28.0,
            columns: 1,
            ..Default::default()
        };
        let rect = Rect::from_min_max(pos2(10.0, 30.0), pos2(40.0, 90.0));
        assert_eq!(rows_in(&list, rect, 100), [1, 2, 3]);
        assert_eq!(rows_in(&list, rect, 3), [1, 2]);

        let grid = ViewState {
            row_pitch: 108.0,
            row_gap: 8.0,
            columns: 3,
            tiles: Some(GridGeometry {
                left: 8.0,
                tile: vec2(100.0, 100.0),
                gap: vec2(8.0, 8.0),
            }),
            ..Default::default()
        };
        // Second column of the first two rows.
        let rect = Rect::from_min_max(pos2(120.0, 50.0), pos2(150.0, 150.0));
        assert_eq!(rows_in(&grid, rect, 100), [1, 4]);
        // Only the gap under the first row: the second row.
        let rect = Rect::from_min_max(pos2(120.0, 102.0), pos2(150.0, 120.0));
        assert_eq!(rows_in(&grid, rect, 100), [4]);
    }
}
