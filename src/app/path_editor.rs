//! The address bar while editing: the path field and the dropdown of folders
//! under it. Up and Down choose a row, Tab completes it and lists the next
//! level, Enter opens it, and a click goes straight there. A bare name finds
//! favorite and recent folders, and pressing anywhere else closes the editor.
//!
//! CREDITS: grew out of <https://github.com/JakeHandsome/egui_autocomplete>
use super::{
    ActionToPerform,
    commands::TabAction,
    directory_path_info::{DirectoryPathInfo, RowKind, offset_to_reveal},
    dock::TabId,
    ui::{self, Palette},
};
use egui::{
    FontId, Key, Modifiers, Popup, PopupCloseBehavior, Response, TextFormat, Ui, text::LayoutJob,
    vec2,
};
use lucide_icons::Icon;
use std::path::{Path, PathBuf};

const VISIBLE_ROWS: usize = 8;
const MIN_POPUP_WIDTH: f32 = 280.0;
const MAX_POPUP_WIDTH: f32 = 640.0;
/// Between the popup's edge and its header and hints.
const SIDE_PADDING: f32 = 8.0;
const HINTS: &str = "Up/Down to choose · Tab to complete · Enter to open · Esc to close";

/// A row that was clicked.
struct Clicked {
    path: PathBuf,
    new_tab: bool,
}

/// A row of the dropdown.
struct Row<'a> {
    name: &'a str,
    indices: &'a [usize],
    kind: RowKind,
    location: Option<String>,
    selected: bool,
}

/// Draws the path field filling `ui`, with its dropdown. Returns whether the
/// editor should close at once because the pointer was pressed outside it.
#[must_use]
pub fn show(
    index: TabId,
    info: &mut DirectoryPathInfo,
    current_path: &Path,
    show_hidden: bool,
    ui: &mut Ui,
) -> bool {
    let id = egui::Id::unique(("path_editor", index));
    let focused = ui.memory(|memory| memory.has_focus(id));
    if focused && info.take_select_all() {
        select_all(ui.ctx(), id, &info.text_input);
    }
    let (down, up, tab) = if focused {
        ui.input_mut(|input| {
            (
                input.consume_key(Modifiers::NONE, Key::ArrowDown),
                input.consume_key(Modifiers::NONE, Key::ArrowUp),
                input.consume_key(Modifiers::NONE, Key::Tab),
            )
        })
    } else {
        (false, false, false)
    };
    if tab && let Some(completed) = info.completion() {
        info.text_input = completed;
        info.mark_edited();
        move_caret_to_end(ui.ctx(), id, &info.text_input);
    }
    let accent = ui.visuals().selection.stroke;
    let response = ui
        .scope(|ui| {
            // egui draws selected text in this color. The accent is hard to
            // read on the selection's fill, and the field opens selected.
            ui.visuals_mut().selection.stroke.color = Palette::of(ui).text;
            ui.add(
                ui::text_field(ui, &mut info.text_input)
                    .id(id)
                    .margin(field_margin(ui))
                    // Tab completes here rather than moving to the next widget.
                    .lock_focus(true)
                    .desired_width(ui.available_width()),
            )
            .accessible_name("Folder path")
        })
        .inner;
    // It also draws the focus ring in that color, so put the accent back.
    if response.has_focus() {
        ui.painter().rect_stroke(
            response.rect,
            ui.visuals().widgets.inactive.corner_radius,
            accent,
            egui::StrokeKind::Inside,
        );
    }
    if response.changed() {
        info.mark_edited();
    }
    let row_height = ui::control_height(ui);
    let viewport = row_height * VISIBLE_ROWS as f32;
    let scroll_to = if info.refresh(show_hidden) {
        Some(0.0)
    } else if info.move_selection(down, up) {
        info.selected
            .map(|row| offset_to_reveal(row, row_height, viewport, info.scroll_offset))
    } else {
        None
    };
    let (clicked, popup) = dropdown(ui, id, &response, info, scroll_to);
    if !response.has_focus() && !ui.ctx().memory(|m| m.focused().is_some()) {
        response.request_focus();
    }
    if ui.input(|i| i.key_pressed(Key::Escape)) {
        ActionToPerform::ToggleTopEdit.schedule();
    } else if let Some(clicked) = clicked {
        if clicked.new_tab {
            // Closed first: the new tab becomes the current one.
            ActionToPerform::ToggleTopEdit.schedule();
            if let Some(action) =
                ActionToPerform::path_from_str(clicked.path.to_string_lossy(), true)
            {
                action.schedule();
            }
        } else {
            open(index, info, current_path, clicked.path);
        }
    } else if focused && ui.input(|i| i.key_pressed(Key::Enter)) {
        let target = info.target();
        open(index, info, current_path, target);
    }
    ui.input(|i| {
        i.pointer.any_pressed()
            && i.pointer.interact_pos().is_some_and(|pos| {
                !response.rect.contains(pos) && !popup.is_some_and(|rect| rect.contains(pos))
            })
    })
}

/// Goes to `target` and closes the editor.
fn open(index: TabId, info: &mut DirectoryPathInfo, current_path: &Path, target: PathBuf) {
    if !target.is_dir() {
        crate::toast!(Warning, "Directory does not exist");
        return;
    }
    // Keeps the editor alive through the navigation, so the toggle closes it
    // rather than opening it again.
    info.text_input = target.to_string_lossy().into_owned();
    if target != current_path {
        TabAction::ChangePaths(crate::app::dock::existing_dir(target)).schedule_tab(index);
    }
    ActionToPerform::ToggleTopEdit.schedule();
}

/// The text starts where the first breadcrumb's does, so it stays put when
/// the bar switches between the two.
#[allow(clippy::cast_possible_truncation)]
fn field_margin(ui: &Ui) -> egui::Margin {
    let x = 4.0 + ui.spacing().button_padding.x;
    egui::Margin::symmetric(x as i8, 2)
}

fn select_all(ctx: &egui::Context, id: egui::Id, text: &str) {
    use egui::text::{CCursor, CCursorRange};
    let mut state = egui::TextEdit::load_state(ctx, id).unwrap_or_default();
    let end = CCursor::new(text.chars().count());
    state
        .cursor
        .set_char_range(Some(CCursorRange::two(CCursor::new(0), end)));
    egui::TextEdit::store_state(ctx, id, state);
}

fn move_caret_to_end(ctx: &egui::Context, id: egui::Id, text: &str) {
    use egui::text::{CCursor, CCursorRange};
    let mut state = egui::TextEdit::load_state(ctx, id).unwrap_or_default();
    let end = CCursor::new(text.chars().count());
    state.cursor.set_char_range(Some(CCursorRange::one(end)));
    egui::TextEdit::store_state(ctx, id, state);
}

fn dropdown(
    ui: &Ui,
    id: egui::Id,
    field: &Response,
    info: &mut DirectoryPathInfo,
    scroll_to: Option<f32>,
) -> (Option<Clicked>, Option<egui::Rect>) {
    let Some(label) = info.header_label() else {
        return (None, None);
    };
    // Open unless something else took focus. Pressing a row takes focus from
    // the field for a moment, and closing then would lose the click.
    let open = ui.memory(|memory| memory.focused().is_none_or(|focused| focused == id));
    let width = field.rect.width().clamp(MIN_POPUP_WIDTH, MAX_POPUP_WIDTH);
    let inner_width = width - ui.spacing().menu_margin.sum().x;
    let shown = Popup::menu(field)
        .id(id.with("dropdown"))
        .open(open)
        .close_behavior(PopupCloseBehavior::IgnoreClicks)
        .gap(4.0)
        .width(width)
        .show(|ui| {
            ui.set_width(inner_width);
            ui.spacing_mut().item_spacing.y = 0.0;
            header(ui, &label, info.count_label().as_deref());
            ui.add(egui::Separator::default().spacing(1.0));
            let clicked = rows(ui, info, scroll_to);
            ui.add(egui::Separator::default().spacing(1.0));
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                ui.add_space(SIDE_PADDING);
                ui.add(egui::Label::new(egui::RichText::new(HINTS).small().weak()).truncate());
            });
            ui.add_space(2.0);
            clicked
        });
    shown.map_or((None, None), |shown| {
        (shown.inner, Some(shown.response.rect))
    })
}

/// One line of `text` that ends in "…" when wider than `max_width`.
const fn one_line(mut job: LayoutJob, max_width: f32) -> LayoutJob {
    job.wrap.max_width = max_width.max(0.0);
    job.wrap.max_rows = 1;
    job.wrap.break_anywhere = true;
    job
}

/// The folder being listed and how many of its folders are shown.
fn header(ui: &mut Ui, label: &str, count: Option<&str>) {
    let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), 26.0), egui::Sense::hover());
    let weak = Palette::of(ui).text_weak;
    let font = egui::TextStyle::Small.resolve(ui.style());
    let right = rect.right() - SIDE_PADDING;
    let count_width = count.map_or(0.0, |count| {
        ui.painter()
            .text(
                egui::pos2(right, rect.center().y),
                egui::Align2::RIGHT_CENTER,
                count,
                font.clone(),
                weak,
            )
            .width()
    });
    let max_width = 2.0f32.mul_add(-SIDE_PADDING, rect.width()) - count_width - 12.0;
    let job = one_line(
        LayoutJob::simple_singleline(label.to_owned(), font, weak),
        max_width,
    );
    let galley = ui.fonts_mut(|fonts| fonts.layout_job(job));
    ui.painter().galley(
        egui::pos2(
            rect.left() + SIDE_PADDING,
            rect.center().y - galley.size().y / 2.0,
        ),
        galley,
        weak,
    );
}

fn rows(ui: &mut Ui, info: &mut DirectoryPathInfo, scroll_to: Option<f32>) -> Option<Clicked> {
    let row_height = ui::control_height(ui);
    if info.matches().is_empty() {
        ui.add_sized(
            [ui.available_width(), row_height],
            egui::Label::new(egui::RichText::new(info.empty_label()).weak()),
        );
        return None;
    }
    let mut clicked = None;
    let mut area = egui::ScrollArea::vertical()
        .id_salt("rows")
        .max_height(row_height * VISIBLE_ROWS as f32)
        .auto_shrink([false, true]);
    if let Some(offset) = scroll_to {
        area = area.vertical_scroll_offset(offset);
    }
    let count = info.matches().len();
    let selected = info.selected;
    let list: &DirectoryPathInfo = info;
    let output = area.show_rows(ui, row_height, count, |ui, range| {
        for index in range {
            let found = &list.matches()[index];
            let response = row(
                ui,
                &Row {
                    name: list.name_of(found),
                    indices: &found.indices,
                    kind: list.kind_of(found),
                    location: list.location_of(found),
                    selected: selected == Some(index),
                },
            );
            let new_tab = response.middle_clicked()
                || (response.clicked() && ui.input(|i| i.modifiers.command));
            if response.clicked() || new_tab {
                clicked = Some(Clicked {
                    path: list.path_of(found).to_path_buf(),
                    new_tab,
                });
            }
        }
    });
    info.scroll_offset = output.state.clamped_offset().y;
    clicked
}

/// A folder's row, drawn like the sidebar's. Selection is shown by the fill,
/// so matched letters on it are underlined rather than colored.
fn row(ui: &mut Ui, row: &Row<'_>) -> Response {
    let (rect, response) = ui.allocate_exact_size(
        vec2(ui.available_width(), ui::control_height(ui)),
        egui::Sense::click(),
    );
    response.widget_info(|| {
        egui::WidgetInfo::selected(egui::Role::Button, true, row.selected, row.name)
    });
    if row.selected || response.hovered() {
        ui.painter().rect_filled(
            rect,
            ui::control_radius(ui),
            ui::navigation_fill(ui, row.selected),
        );
    }
    let palette = Palette::of(ui);
    let icon = match row.kind {
        RowKind::Subfolder => Icon::Folder,
        RowKind::Favorite => Icon::Star,
        RowKind::Recent => Icon::Clock,
    };
    ui.painter().text(
        rect.left_center() + vec2(16.0, 0.0),
        egui::Align2::CENTER_CENTER,
        char::from(icon),
        FontId::new(14.0, egui::FontFamily::Name("lucide".into())),
        if row.selected {
            palette.text
        } else {
            palette.text_weak
        },
    );
    let font = egui::TextStyle::Body.resolve(ui.style());
    let plain = TextFormat::simple(font.clone(), palette.text);
    let hit = if row.selected {
        TextFormat {
            underline: egui::Stroke::new(1.0_f32, palette.text),
            ..plain.clone()
        }
    } else {
        TextFormat::simple(font, palette.accent)
    };
    let job = one_line(
        highlight_matches(row.name, row.indices, &plain, &hit),
        rect.width() - 40.0,
    );
    let galley = ui.fonts_mut(|fonts| fonts.layout_job(job));
    let name_end = rect.left() + 32.0 + galley.size().x;
    ui.painter().galley(
        rect.left_center() + vec2(32.0, -galley.size().y / 2.0),
        galley,
        palette.text,
    );
    // The name comes first; its location takes what is left, if it is enough
    // to read.
    let room = rect.right() - SIDE_PADDING - name_end - 16.0;
    if let Some(location) = row.location.as_ref().filter(|_| room > 60.0) {
        let job = one_line(
            LayoutJob::simple_singleline(
                location.clone(),
                egui::TextStyle::Small.resolve(ui.style()),
                palette.text_weak,
            ),
            room,
        );
        let galley = ui.fonts_mut(|fonts| fonts.layout_job(job));
        ui.painter().galley(
            egui::pos2(
                rect.right() - SIDE_PADDING - galley.size().x,
                rect.center().y - galley.size().y / 2.0,
            ),
            galley,
            palette.text_weak,
        );
    }
    response
}

/// Lays out `text` with its characters at `match_indices` in `hit`.
fn highlight_matches(
    text: &str,
    match_indices: &[usize],
    plain: &TextFormat,
    hit: &TextFormat,
) -> LayoutJob {
    let mut formatted = LayoutJob::default();
    let mut it = text.char_indices().enumerate().peekable();
    // Iterate through all indices in the string
    while let Some((char_idx, (byte_idx, c))) = it.next() {
        let start = byte_idx;
        let mut end = byte_idx + (c.len_utf8() - 1);
        let match_state = match_indices.contains(&char_idx);
        // Find all consecutive characters that have the same state
        while let Some((peek_char_idx, (_, k))) = it.peek() {
            if match_state == match_indices.contains(peek_char_idx) {
                end += k.len_utf8();
                // Advance the iterator, we already peeked the value so it is fine to ignore
                _ = it.next();
            } else {
                break;
            }
        }
        let format = if match_state { hit } else { plain };
        formatted.append(&text[start..=end], 0.0, format.clone());
    }
    formatted
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn highlight() {
        let text = String::from("Test123áéíó");
        let plain = TextFormat::default();
        let hit = TextFormat::simple(FontId::default(), egui::Color32::RED);
        let layout = highlight_matches(&text, &[1, 5, 6, 8, 9, 10], &plain, &hit);
        let sections: Vec<(&str, bool)> = layout
            .sections
            .iter()
            .map(|section| {
                (
                    &text[section.byte_range.start.0..section.byte_range.end.0],
                    section.format.color == egui::Color32::RED,
                )
            })
            .collect();
        assert_eq!(
            sections,
            [
                ("T", false),
                ("e", true),
                ("st1", false),
                ("23", true),
                ("á", false),
                ("éíó", true),
            ]
        );
    }
}
