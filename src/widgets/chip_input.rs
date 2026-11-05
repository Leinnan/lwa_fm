//! A search box that shows the parts of a search as pills (chips) in front
//! of the text being typed, all inside one field.
//!
//! Mouse: click a pill to edit it; its ×, a middle-click or its right-click
//! menu removes it; the × at the end of the field clears everything.
//! Keyboard: Enter turns the text into pills and a space after `key:value`
//! makes a filter pill. With the cursor at the start of the text, ← or
//! Backspace selects the last pill; then ← → move, Delete / Backspace
//! remove, Enter / F2 / Space edit and Esc goes back to the text.
//!
//! A pill's editor works on the pill itself, so results update while you
//! edit, but it becomes one undo step only when the editor closes. Esc puts
//! the pill back as it was; a new pill left blank is dropped.

use crate::app::search::filter::pattern_error;
use crate::app::search::query::{chip_label, chip_parts, chip_token, cmp_words, parse_filter};
use crate::app::search::{Age, Chip, Cmp, FileKind, Search, SearchTermType};
use crate::app::ui::{self, Palette};
use egui::text::{CharIndex, LayoutJob, TextFormat, TextWrapping};
use egui::{
    Color32, Context, CursorIcon, FontFamily, FontId, Id, Key, Modifiers, Rect, Response, Sense,
    Stroke, StrokeKind, TextStyle, Ui, Vec2, WidgetInfo, pos2, vec2,
};
use lucide_icons::Icon;
use std::path::{Path, PathBuf};

/// Pill labels longer than this are cut with an ellipsis (the tooltip and
/// the editor show the whole value).
const MAX_PILL_TEXT: f32 = 220.0;

/// What changed in the chip input this frame.
#[derive(Default)]
pub struct ChipInputOutput {
    /// The typed text changed (filter live, record later).
    pub text_changed: bool,
    /// A pill is being edited: filter live, but don't record it yet.
    pub preview: bool,
    /// The chips changed; the label describes it for undo ("add filter …").
    pub chips_changed: Option<String>,
    /// Enter was pressed with nothing left to turn into chips.
    pub submitted: bool,
    /// The text box, for focus handling by the caller.
    pub text: Option<Response>,
}

/// Suggestions for the pill editors.
#[derive(Default)]
pub struct ChipContext<'a> {
    /// Extensions found where the search runs, most common first.
    pub extensions: &'a [String],
    /// Quick picks for the folder editor: (label, path).
    pub folders: &'a [(String, PathBuf)],
}

/// The pill being edited. Kept in egui's temporary data between frames.
#[derive(Clone)]
struct Editing {
    index: usize,
    /// The pill before editing, for Esc.
    original: Chip,
    /// Added from the "+ Filter" menu: dropped if left blank or cancelled.
    is_new: bool,
    /// Focus the editor's first field on the next frame.
    focus: bool,
    /// The extension or folder text as typed, which may not parse yet.
    text: String,
    /// The size as shown: a number and its unit in bytes.
    size_number: f64,
    size_unit: u64,
}

const KB: u64 = 1 << 10;
const MB: u64 = 1 << 20;
const GB: u64 = 1 << 30;

impl Editing {
    fn new(index: usize, chip: &Chip, is_new: bool) -> Self {
        let text = match chip {
            Chip::Ext(exts) => exts.join(", "),
            Chip::In(path) => path.display().to_string(),
            _ => String::new(),
        };
        let (size_number, size_unit) = match chip {
            Chip::Size { bytes, .. } => {
                let unit = [GB, MB]
                    .into_iter()
                    .find(|unit| bytes >= unit)
                    .unwrap_or(KB);
                (*bytes as f64 / unit as f64, unit)
            }
            _ => (10.0, MB),
        };
        Self {
            index,
            original: chip.clone(),
            is_new,
            focus: true,
            text,
            size_number,
            size_unit,
        }
    }
}

/// Opens the editor of the pill at `index` on the next frame; `is_new`
/// marks a pill just added, so cancelling removes it again.
pub fn edit_chip(ctx: &Context, id: Id, index: usize, is_new: bool) {
    ctx.data_mut(|d| d.insert_temp(id.with("edit_request"), Some((index, is_new))));
    ctx.request_repaint();
}

/// Whether a pill editor is open (so the caller can gather suggestions only
/// when they are needed).
pub fn is_editing(ctx: &Context, id: Id) -> bool {
    ctx.data(|d| d.get_temp::<Option<Editing>>(id.with("editing")))
        .flatten()
        .is_some()
}

/// Something done to the pills this frame.
#[derive(Clone, Copy)]
enum Action {
    Remove(usize),
    /// Removed with the keyboard: select a neighbour (the previous one when
    /// going backwards) so the next key press continues.
    RemoveKey(usize, bool),
    RemoveAll,
    Edit(usize),
    ToggleExclude(usize),
    Clear,
}

/// How a pill editor closes.
#[derive(PartialEq, Eq)]
enum Close {
    Apply,
    Revert,
    Remove,
}

/// At least 24px, the smallest target WCAG 2.5.8 allows.
fn pill_height(ui: &Ui) -> f32 {
    ui::control_height(ui) - 4.0
}

fn chip_icon(chip: &Chip, invalid: bool) -> Option<Icon> {
    if invalid {
        return Some(Icon::TriangleAlert);
    }
    Some(match chip {
        Chip::Name { exclude: true, .. } => Icon::Ban,
        Chip::Name {
            kind: SearchTermType::Regex,
            ..
        } => Icon::Regex,
        Chip::Name {
            kind: SearchTermType::Glob,
            ..
        } => Icon::Asterisk,
        Chip::Name { .. } => return None,
        Chip::Ext(_) => Icon::FileType,
        Chip::Kind(kinds) => match kinds.first() {
            Some(FileKind::Folder) => Icon::Folder,
            Some(FileKind::Image) => Icon::Image,
            Some(FileKind::Video) => Icon::Film,
            Some(FileKind::Audio) => Icon::Music,
            Some(FileKind::Document) => Icon::FileText,
            Some(FileKind::Archive) => Icon::Archive,
            None => Icon::Shapes,
        },
        Chip::Size { .. } => Icon::HardDrive,
        Chip::Modified(_) | Chip::Opened(_) => Icon::Calendar,
        Chip::In(_) => Icon::FolderSearch,
    })
}

fn icon_font(size: f32) -> FontId {
    FontId::new(size, FontFamily::Name("lucide".into()))
}

/// The two parts of a pill: the body (edit) and its × (remove). They are
/// separate, non-overlapping hit areas, so the × is never covered.
struct Pill {
    body: Response,
    remove: Response,
}

/// The pill's text: a muted "Key:" and the value.
fn pill_text(ui: &Ui, chip: &Chip) -> std::sync::Arc<egui::Galley> {
    let palette = Palette::of(ui);
    let (key, value) = chip_parts(chip);
    let font = TextStyle::Body.resolve(ui.style());
    let mut job = LayoutJob::default();
    if let Some(key) = key {
        job.append(
            &format!("{key}:"),
            0.0,
            TextFormat::simple(font.clone(), palette.text_weak),
        );
    }
    let (value, value_color) = if value.trim().is_empty() {
        ("…".to_owned(), palette.text_weak)
    } else {
        (value, palette.text)
    };
    job.append(
        &value,
        if key.is_some() { 4.0 } else { 0.0 },
        TextFormat::simple(font, value_color),
    );
    job.wrap = TextWrapping::truncate_at_width(MAX_PILL_TEXT);
    ui.painter().layout_job(job)
}

/// Draws one pill: `[icon] Key: value [×]`.
fn draw_chip(ui: &mut Ui, id: Id, chip: &Chip, selected: bool) -> Pill {
    let palette = Palette::of(ui);
    let height = pill_height(ui);
    let invalid = match chip {
        Chip::Name { pattern, kind, .. } => pattern_error(pattern, *kind),
        _ => None,
    };
    let galley = pill_text(ui, chip);
    let error = ui.visuals().error_fg_color;
    let icon = chip_icon(chip, invalid.is_some()).map(|icon| {
        let color = if invalid.is_some() || matches!(chip, Chip::Name { exclude: true, .. }) {
            error
        } else {
            palette.text_weak
        };
        ui.painter()
            .layout_no_wrap(char::from(icon).to_string(), icon_font(13.0), color)
    });

    let pad = 8.0;
    let icon_width = icon.as_ref().map_or(0.0, |icon| icon.size().x + 5.0);
    let remove_width = height;
    let width = pad + icon_width + galley.size().x + 2.0 + remove_width;
    let (rect, _) = ui.allocate_exact_size(vec2(width, height), Sense::hover());
    let remove_rect = Rect::from_min_max(pos2(rect.right() - remove_width, rect.top()), rect.max);
    let body_rect = Rect::from_min_max(rect.min, pos2(remove_rect.left(), rect.bottom()));
    let body = ui.interact(body_rect, id.with("body"), Sense::click());
    let remove = ui.interact(remove_rect, id.with("remove"), Sense::click());

    let label = chip_label(chip);
    body.widget_info(|| WidgetInfo::labeled(egui::Role::Button, true, label.clone()));
    remove.widget_info(|| WidgetInfo::labeled(egui::Role::Button, true, format!("Remove {label}")));

    if ui.is_rect_visible(rect) {
        let hovered = body.hovered() || remove.hovered();
        let fill = if matches!(chip, Chip::Name { .. }) {
            let base = palette.border;
            if hovered {
                base.lerp_to_gamma(palette.text, 0.1)
            } else {
                base
            }
        } else {
            palette
                .accent
                .gamma_multiply(if hovered { 0.34 } else { 0.22 })
        };
        let stroke = if selected {
            Stroke::new(2.0, palette.accent)
        } else if invalid.is_some() || matches!(chip, Chip::Name { exclude: true, .. }) {
            Stroke::new(1.0, error)
        } else {
            Stroke::NONE
        };
        let radius = ui::control_radius(ui);
        let painter = ui.painter();
        painter.rect(rect, radius, fill, stroke, StrokeKind::Inside);
        let mut x = rect.left() + pad;
        if let Some(icon) = icon {
            let y = rect.center().y - icon.size().y / 2.0;
            painter.galley(pos2(x, y), icon, palette.text_weak);
            x += icon_width;
        }
        let y = rect.center().y - galley.size().y / 2.0;
        painter.galley(pos2(x, y), galley, palette.text);
        let x_color = if remove.hovered() {
            painter.circle_filled(
                remove_rect.center(),
                height * 0.36,
                palette.text.gamma_multiply(0.16),
            );
            palette.text
        } else {
            palette.text_weak
        };
        painter.text(
            remove_rect.center(),
            egui::Align2::CENTER_CENTER,
            char::from(Icon::X),
            icon_font(12.0),
            x_color,
        );
    }

    let tip = invalid.map_or_else(
        || {
            format!(
                "{}\nClick to edit · Middle-click to remove",
                chip_token(chip)
            )
        },
        |error| format!("Invalid pattern: {error}"),
    );
    Pill {
        body: body
            .on_hover_cursor(CursorIcon::PointingHand)
            .on_hover_text(tip),
        remove: remove
            .on_hover_cursor(CursorIcon::PointingHand)
            .on_hover_text("Remove"),
    }
}

/// Whether the text cursor sits at the very start of the box, so ← and
/// Backspace move on to the pills instead of the text.
fn cursor_at_start(ctx: &Context, id: Id, text: &str) -> bool {
    text.is_empty()
        || egui::text_edit::TextEditState::load(ctx, id)
            .and_then(|state| state.cursor.char_range())
            .is_some_and(|range| {
                range.primary.index == CharIndex(0) && range.secondary.index == CharIndex(0)
            })
}

/// Keys for the pills, read before the text box so it doesn't also act on
/// them. Only called while the text box has focus.
fn pill_keys(
    ui: &Ui,
    id: Id,
    text: &str,
    count: usize,
    selected: &mut Option<usize>,
) -> Option<Action> {
    let at_start = cursor_at_start(ui.ctx(), id, text);
    ui.input_mut(|input| {
        let mut take = |key| input.consume_key(Modifiers::NONE, key);
        match *selected {
            Some(index) => {
                if take(Key::ArrowLeft) {
                    *selected = Some(index.saturating_sub(1));
                } else if take(Key::ArrowRight) {
                    *selected = (index + 1 < count).then_some(index + 1);
                } else if take(Key::Home) {
                    *selected = Some(0);
                } else if take(Key::End) || take(Key::Escape) {
                    *selected = None;
                } else if take(Key::Backspace) {
                    return Some(Action::RemoveKey(index, true));
                } else if take(Key::Delete) {
                    return Some(Action::RemoveKey(index, false));
                } else if take(Key::Enter) || take(Key::F2) || take(Key::Space) {
                    return Some(Action::Edit(index));
                } else if input
                    .events
                    .iter()
                    .any(|event| matches!(event, egui::Event::Text(_)))
                {
                    *selected = None;
                }
            }
            None if at_start && (take(Key::ArrowLeft) || take(Key::Backspace)) => {
                *selected = Some(count - 1);
            }
            None => {}
        }
        None
    })
}

/// The extensions typed in the editor: "pdf, .Docx" → `["pdf", "docx"]`.
fn parse_extensions(text: &str) -> Vec<String> {
    let mut exts: Vec<String> = Vec::new();
    for ext in text.split([',', ' ']) {
        let ext = ext.trim().trim_start_matches('.').to_lowercase();
        if !ext.is_empty() && !exts.contains(&ext) {
            exts.push(ext);
        }
    }
    exts
}

/// A text field that takes focus when the editor opens and reports Enter.
fn editor_field(
    ui: &mut Ui,
    name: &str,
    text: &mut String,
    hint: &str,
    focus: &mut bool,
) -> (bool, bool) {
    let response = ui
        .add(
            ui::text_field(ui, text)
                .hint_text(hint)
                .desired_width(f32::INFINITY),
        )
        .accessible_name(name);
    if std::mem::take(focus) {
        response.request_focus();
    }
    let enter = response.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter));
    (response.changed(), enter)
}

/// The editor shown for a pill. Returns how it asks to close, if it does.
fn chip_editor(
    ui: &mut Ui,
    chip: &mut Chip,
    editing: &mut Editing,
    context: &ChipContext<'_>,
) -> Option<Close> {
    ui.set_min_width(260.0);
    ui.set_max_width(320.0);
    let title = match chip {
        Chip::Name { exclude: true, .. } => "Exclude names containing",
        Chip::Name { .. } => "Name contains",
        Chip::Ext(_) => "Extension",
        Chip::Kind(_) => "Type",
        Chip::Size { .. } => "Size",
        Chip::Modified(_) => "Modified",
        Chip::Opened(_) => "Opened",
        Chip::In(_) => "Also search in folder",
    };
    ui.strong(title);
    ui.add_space(2.0);
    // Enter in a text field applies, like the Done button.
    let enter = match chip {
        Chip::Name {
            pattern,
            kind,
            exclude,
        } => name_editor(ui, pattern, kind, exclude, &mut editing.focus),
        Chip::Kind(kinds) => {
            ui.weak("Items of any of the checked types");
            for kind in FileKind::ALL {
                let mut on = kinds.contains(&kind);
                if ui.checkbox(&mut on, kind.label()).changed() {
                    if on {
                        kinds.push(kind);
                    } else {
                        kinds.retain(|k| *k != kind);
                    }
                }
            }
            false
        }
        Chip::Modified(age) | Chip::Opened(age) => {
            for value in Age::ALL {
                ui.radio_value(age, value, value.label());
            }
            false
        }
        Chip::Size { cmp, bytes } => {
            size_editor(ui, cmp, editing);
            *bytes = (editing.size_number.max(0.0) * editing.size_unit as f64) as u64;
            false
        }
        Chip::Ext(exts) => extension_editor(ui, exts, editing, context.extensions),
        Chip::In(path) => folder_editor(ui, path, editing, context.folders),
    };
    ui.add_space(4.0);
    ui.separator();
    let mut close = enter.then_some(Close::Apply);
    ui.horizontal(|ui| {
        if ui
            .button("Remove")
            .on_hover_text("Remove this filter")
            .clicked()
        {
            close = Some(Close::Remove);
        }
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            let done = egui::Button::new(egui::RichText::new("Done").color(Color32::WHITE))
                .fill(Palette::of(ui).accent);
            if ui.add(done).on_hover_text("Enter").clicked() {
                close = Some(Close::Apply);
            }
        });
    });
    close
}

/// Editor for a word: the text, how it matches, and include / exclude.
/// Returns whether Enter was pressed.
fn name_editor(
    ui: &mut Ui,
    pattern: &mut String,
    kind: &mut SearchTermType,
    exclude: &mut bool,
    focus: &mut bool,
) -> bool {
    let hint = match kind {
        SearchTermType::Plain => "Text",
        SearchTermType::Glob => "e.g. IMG_*.jpg",
        SearchTermType::Regex => "e.g. ^IMG_\\d+",
    };
    let (_, enter) = editor_field(ui, "Name pattern", pattern, hint, focus);
    ui.horizontal(|ui| {
        for (value, label) in [
            (SearchTermType::Plain, "Text"),
            (SearchTermType::Glob, "Wildcard"),
            (SearchTermType::Regex, "Regex"),
        ] {
            ui.selectable_value(kind, value, label);
        }
    });
    ui.checkbox(exclude, "Exclude matching items");
    if let Some(error) = pattern_error(pattern, *kind) {
        ui.colored_label(ui.visuals().error_fg_color, error);
    }
    enter
}

/// Editor for a size: "over / under", a number and a unit.
fn size_editor(ui: &mut Ui, cmp: &mut Cmp, editing: &mut Editing) {
    ui.horizontal(|ui| {
        egui::ComboBox::from_id_salt("size_cmp")
            .selected_text(cmp_words(*cmp))
            .width(80.0)
            .show_ui(ui, |ui| {
                for value in [
                    Cmp::Greater,
                    Cmp::GreaterOrEqual,
                    Cmp::Less,
                    Cmp::LessOrEqual,
                    Cmp::Equal,
                ] {
                    ui.selectable_value(cmp, value, cmp_words(value));
                }
            })
            .response
            .accessible_name("Size comparison");
        let number = ui
            .add(
                egui::DragValue::new(&mut editing.size_number)
                    .range(0.0..=f64::from(u32::MAX))
                    .speed(1.0)
                    .max_decimals(1),
            )
            .accessible_name("Size");
        if std::mem::take(&mut editing.focus) {
            number.request_focus();
        }
        egui::ComboBox::from_id_salt("size_unit")
            .selected_text(match editing.size_unit {
                GB => "GB",
                MB => "MB",
                _ => "KB",
            })
            .width(56.0)
            .show_ui(ui, |ui| {
                for (unit, label) in [(KB, "KB"), (MB, "MB"), (GB, "GB")] {
                    ui.selectable_value(&mut editing.size_unit, unit, label);
                }
            })
            .response
            .accessible_name("Size unit");
    });
}

/// Editor for extensions: a comma list plus the common extensions found
/// where the search runs. Returns whether Enter was pressed.
fn extension_editor(
    ui: &mut Ui,
    exts: &mut Vec<String>,
    editing: &mut Editing,
    found: &[String],
) -> bool {
    let (changed, enter) = editor_field(
        ui,
        "Extensions",
        &mut editing.text,
        "e.g. pdf, docx",
        &mut editing.focus,
    );
    if changed {
        *exts = parse_extensions(&editing.text);
    }
    let suggestions: Vec<&String> = found
        .iter()
        .filter(|ext| !exts.contains(ext))
        .take(8)
        .collect();
    if !suggestions.is_empty() {
        ui.add_space(2.0);
        ui.weak("Found here");
        ui.horizontal_wrapped(|ui| {
            for ext in suggestions {
                if ui.small_button(format!(".{ext}")).clicked() {
                    exts.push(ext.clone());
                    editing.text = exts.join(", ");
                }
            }
        });
    }
    enter
}

/// Editor for an extra folder: a path, quick picks and (on Windows) the
/// system folder picker. Returns whether Enter was pressed.
fn folder_editor(
    ui: &mut Ui,
    path: &mut PathBuf,
    editing: &mut Editing,
    folders: &[(String, PathBuf)],
) -> bool {
    let (changed, enter) = editor_field(
        ui,
        "Folder path",
        &mut editing.text,
        "Folder path",
        &mut editing.focus,
    );
    if changed {
        *path = PathBuf::from(editing.text.trim());
    }
    if !path.as_os_str().is_empty() && !path.is_dir() {
        ui.colored_label(ui.visuals().warn_fg_color, "Folder not found");
    }
    ui.horizontal_wrapped(|ui| {
        for (label, folder) in folders {
            if ui
                .small_button(label)
                .on_hover_text(folder.display().to_string())
                .clicked()
            {
                path.clone_from(folder);
                editing.text = folder.display().to_string();
            }
        }
        #[cfg(any(windows, target_os = "macos"))]
        if ui.small_button("Browse…").clicked() {
            let start = Some(path.as_path()).filter(|p| p.is_dir());
            if let Some(folder) = crate::shell::pick_folder(start) {
                editing.text = folder.display().to_string();
                *path = folder;
            }
        }
    });
    enter
}

/// Moves every complete `key:value` token (one followed by whitespace) out
/// of the text and into chips. Returns the tokens that became chips.
fn extract_filters(search: &mut Search) -> Vec<String> {
    let text = search.value.clone();
    let mut kept = String::new();
    let mut moved = Vec::new();
    let mut rest = text.as_str();
    while let Some(start) = rest.find(|c: char| !c.is_whitespace()) {
        kept.push_str(&rest[..start]);
        rest = &rest[start..];
        let end = rest.find(char::is_whitespace);
        let token = &rest[..end.unwrap_or(rest.len())];
        match (end, parse_filter(token)) {
            (Some(_), Some(chip)) if !token.starts_with('"') => {
                moved.push(chip_token(&chip));
                search.add_chip(chip);
            }
            _ => kept.push_str(token),
        }
        rest = &rest[token.len()..];
    }
    kept.push_str(rest);
    if !moved.is_empty() {
        // Collapse the gaps left by removed tokens, keeping a trailing space
        // so typing continues naturally.
        let mut tidy = kept.split_whitespace().collect::<Vec<_>>().join(" ");
        if text.ends_with(char::is_whitespace) && !tidy.is_empty() {
            tidy.push(' ');
        }
        search.value = tidy;
    }
    moved
}

/// Closes the editor of the pill at `editing.index`, applying, reverting or
/// removing it.
fn finish_editing(
    search: &mut Search,
    editing: Editing,
    close: &Close,
    output: &mut ChipInputOutput,
) {
    let index = editing.index;
    output.preview = true;
    let remove = *close == Close::Remove || search.chips[index].is_blank();
    if *close == Close::Revert {
        if editing.is_new {
            search.chips.remove(index);
        } else {
            search.chips[index] = editing.original;
        }
    } else if remove {
        search.chips.remove(index);
        if !editing.is_new {
            output.chips_changed = Some(format!("remove filter {}", chip_token(&editing.original)));
        }
    } else if editing.is_new {
        // Added from the menu: merge it like any other new filter.
        let chip = search.chips.remove(index);
        output.chips_changed = search.add_chip(chip);
    } else if search.chips[index] != editing.original {
        output.chips_changed = Some(format!("edit filter {}", chip_token(&editing.original)));
    }
}

/// Applies a pill action. Returns the pill to select afterwards.
fn apply_action(
    search: &mut Search,
    action: Action,
    editing: &mut Option<Editing>,
    output: &mut ChipInputOutput,
) -> Option<usize> {
    let token = |search: &Search, index: usize| chip_token(&search.chips[index]);
    match action {
        Action::Remove(index) => {
            output.chips_changed = Some(format!("remove filter {}", token(search, index)));
            search.chips.remove(index);
            None
        }
        Action::RemoveKey(index, backwards) => {
            output.chips_changed = Some(format!("remove filter {}", token(search, index)));
            search.chips.remove(index);
            let left = search.chips.len();
            if left == 0 {
                None
            } else if backwards {
                Some(index.saturating_sub(1))
            } else {
                Some(index.min(left - 1))
            }
        }
        Action::RemoveAll => {
            search.chips.clear();
            output.chips_changed = Some("remove all filters".to_owned());
            None
        }
        Action::Clear => {
            search.chips.clear();
            search.value.clear();
            output.chips_changed = Some("clear search".to_owned());
            None
        }
        Action::Edit(index) => {
            *editing = Some(Editing::new(index, &search.chips[index], false));
            Some(index)
        }
        Action::ToggleExclude(index) => {
            if let Chip::Name { exclude, .. } = &mut search.chips[index] {
                *exclude = !*exclude;
                output.chips_changed = Some(format!(
                    "{} {}",
                    if *exclude { "exclude" } else { "include" },
                    token(search, index)
                ));
            }
            None
        }
    }
}

/// The pill's right-click menu.
fn pill_menu(ui: &mut Ui, chip: &Chip, index: usize, count: usize) -> Option<Action> {
    let mut entries = vec![("Edit…", true, Action::Edit(index))];
    if let Chip::Name { exclude, .. } = chip {
        let label = if *exclude {
            "Include matching items"
        } else {
            "Exclude matching items"
        };
        entries.push((label, true, Action::ToggleExclude(index)));
    }
    entries.push(("Remove", true, Action::Remove(index)));
    entries.push(("Remove all filters", count > 1, Action::RemoveAll));
    let mut chosen = None;
    for (label, enabled, action) in entries {
        if matches!(action, Action::RemoveAll) {
            ui.separator();
        }
        if ui.add_enabled(enabled, egui::Button::new(label)).clicked() {
            chosen = Some(action);
        }
    }
    chosen
}

/// The chip input. `id` must be stable per tab.
#[allow(clippy::too_many_lines)]
pub fn chip_input(
    ui: &mut Ui,
    id: Id,
    search: &mut Search,
    hint: &str,
    context: &ChipContext<'_>,
) -> ChipInputOutput {
    let mut output = ChipInputOutput::default();
    let ctx = ui.ctx().clone();
    let selected_id = id.with("selected_chip");
    let editing_id = id.with("editing");
    let count = search.chips.len();
    let mut selected: Option<usize> = ui
        .data(|d| d.get_temp(selected_id))
        .flatten()
        .filter(|&index| index < count);
    let mut editing: Option<Editing> = ui
        .data(|d| d.get_temp(editing_id))
        .flatten()
        .filter(|editing: &Editing| editing.index < count);
    if let Some((index, is_new)) = ui
        .data_mut(|d| d.remove_temp::<Option<(usize, bool)>>(id.with("edit_request")))
        .flatten()
        .filter(|(index, _)| *index < count)
    {
        editing = Some(Editing::new(index, &search.chips[index], is_new));
        selected = Some(index);
    }

    let text_focused = ui.memory(|m| m.has_focus(id));
    let mut action = None;
    if text_focused && editing.is_none() && count > 0 {
        action = pill_keys(ui, id, &search.value, count, &mut selected);
    } else if !text_focused && editing.is_none() {
        selected = None;
    }

    let palette = Palette::of(ui);
    let height = pill_height(ui);
    let mut pill_rects = Vec::with_capacity(count);
    let field = egui::Frame::new()
        .fill(ui.visuals().text_edit_bg_color())
        .corner_radius(ui::control_radius(ui))
        // Two points above and below the pills: the field is as tall as the
        // other controls.
        .inner_margin(egui::Margin {
            left: 8,
            right: 2,
            top: 2,
            bottom: 2,
        })
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal_wrapped(|ui| {
                ui.spacing_mut().item_spacing = vec2(4.0, 4.0);
                ui.add_sized(
                    vec2(18.0, height),
                    egui::Label::new(ui::glyph(Icon::Search).size(14.0).color(palette.text_weak))
                        .selectable(false),
                );
                for (index, chip) in search.chips.iter().enumerate() {
                    let pill =
                        draw_chip(ui, id.with(("chip", index)), chip, selected == Some(index));
                    if pill.remove.clicked()
                        || pill.body.middle_clicked()
                        || pill.remove.middle_clicked()
                    {
                        action = Some(Action::Remove(index));
                    } else if pill.body.clicked() {
                        action = Some(Action::Edit(index));
                    }
                    pill.body.context_menu(|ui| {
                        if let Some(chosen) = pill_menu(ui, chip, index, count) {
                            action = Some(chosen);
                        }
                    });
                    pill_rects.push(pill.body.rect.union(pill.remove.rect));
                }
                // In a wrapping layout `available_width` is the whole row; use
                // what is left after the pills (less the clear button), and
                // wrap only when that gets too small.
                let clear_width = if search.has_query() {
                    height + 4.0
                } else {
                    0.0
                };
                let left = ui.max_rect().right() - ui.cursor().min.x - clear_width;
                let width = if left >= 120.0 {
                    left
                } else {
                    ui.max_rect().width() - clear_width
                };
                let hint = if selected.is_some() {
                    "Delete to remove · Enter to edit"
                } else if count > 0 {
                    "Add words or filters"
                } else {
                    hint
                };
                let text = ui.add(
                    egui::TextEdit::singleline(&mut search.value)
                        .id(id)
                        .frame(egui::Frame::NONE)
                        .min_size(vec2(0.0, height))
                        .align(egui::Align2::LEFT_CENTER)
                        .hint_text(hint)
                        .desired_width(width.max(40.0)),
                );
                // The hint is not a name; screen readers need one.
                text.widget_info(|| {
                    let mut info =
                        WidgetInfo::text_edit(ui.is_enabled(), &search.value, &search.value, hint);
                    info.label = Some("Search".to_owned());
                    info
                });
                if text.changed() {
                    output.text_changed = true;
                    selected = None;
                    let moved = extract_filters(search);
                    if !moved.is_empty() {
                        output.chips_changed = Some(format!("add filter {}", moved.join(" ")));
                    }
                }
                if text.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter)) {
                    if search.commit_text() {
                        output.chips_changed = Some("add search term".to_owned());
                        text.request_focus();
                    } else {
                        output.submitted = true;
                    }
                }
                if search.has_query() {
                    let clear = ui
                        .add(
                            egui::Button::new(
                                ui::glyph(Icon::X).size(14.0).color(palette.text_weak),
                            )
                            .frame(false)
                            .min_size(Vec2::splat(height)),
                        )
                        .on_hover_cursor(CursorIcon::PointingHand);
                    clear.widget_info(|| {
                        WidgetInfo::labeled(egui::Role::Button, true, "Clear search")
                    });
                    if clear.on_hover_text("Clear search").clicked() {
                        action = Some(Action::Clear);
                    }
                }
                output.text = Some(text);
            });
        });

    // The border shows focus for the whole field, pills included.
    let focused = text_focused || selected.is_some() || editing.is_some();
    let stroke = if focused {
        Stroke::new(1.5, palette.accent)
    } else {
        Stroke::new(1.0, palette.border)
    };
    ui.painter().rect_stroke(
        field.response.rect,
        ui::control_radius(ui),
        stroke,
        StrokeKind::Inside,
    );

    // The open editor, anchored under its pill.
    if let Some(mut state) = editing.take()
        && let Some(&anchor) = pill_rects.get(state.index)
    {
        let before = search.chips[state.index].clone();
        let popup = egui::Popup::new(id.with("chip_editor"), ctx.clone(), anchor, ui.layer_id())
            .kind(egui::PopupKind::Popup)
            .open(true)
            .close_behavior(egui::PopupCloseBehavior::CloseOnClickOutside)
            .show(|ui| chip_editor(ui, &mut search.chips[state.index], &mut state, context));
        if search.chips[state.index] != before {
            output.preview = true;
        }
        let close = popup.and_then(|inner| {
            let escape = ui.input_mut(|i| i.consume_key(Modifiers::NONE, Key::Escape));
            inner.inner.or_else(|| {
                inner.response.should_close().then_some(if escape {
                    Close::Revert
                } else {
                    Close::Apply
                })
            })
        });
        match close {
            Some(close) => {
                finish_editing(search, state, &close, &mut output);
                selected = None;
                ctx.memory_mut(|m| m.request_focus(id));
            }
            None => editing = Some(state),
        }
    }

    if let Some(action) = action {
        if editing.is_some() {
            // A click elsewhere while editing closes the editor first; it was
            // applied above, so act on the pills as they are now.
            editing = None;
        }
        if let Action::Edit(index) | Action::Remove(index) | Action::ToggleExclude(index) = action
            && index >= search.chips.len()
        {
            // The editor that just closed removed this pill.
        } else {
            selected = apply_action(search, action, &mut editing, &mut output);
            if editing.is_none() {
                ctx.memory_mut(|m| m.request_focus(id));
            }
        }
    }
    ui.data_mut(|d| {
        d.insert_temp(selected_id, selected);
        d.insert_temp(editing_id, editing);
    });
    output
}

/// Folder quick picks for the folder editor: the open folder and favorites.
pub fn folder_choices(
    current: Option<&Path>,
    favorites: impl IntoIterator<Item = (String, PathBuf)>,
) -> Vec<(String, PathBuf)> {
    let mut folders: Vec<(String, PathBuf)> = Vec::new();
    if let Some(current) = current {
        folders.push(("Open folder".to_owned(), current.to_path_buf()));
    }
    for (name, path) in favorites {
        if !folders.iter().any(|(_, known)| *known == path) {
            folders.push((name, path));
        }
    }
    folders
}

#[cfg(test)]
mod tests {
    use super::{extract_filters, parse_extensions};
    use crate::app::search::{Chip, FileKind, Search};

    #[test]
    fn complete_filter_tokens_become_chips() {
        let mut search = Search {
            value: "report ext:pdf kind:ima".into(),
            ..Default::default()
        };
        assert_eq!(extract_filters(&mut search), vec!["ext:pdf".to_owned()]);
        assert_eq!(search.value, "report kind:ima");
        assert_eq!(search.chips, vec![Chip::Ext(vec!["pdf".into()])]);
        search.value.push_str("ge ");
        extract_filters(&mut search);
        assert_eq!(search.value, "report ");
        assert_eq!(search.chips.len(), 2);
    }

    #[test]
    fn typed_filters_merge_instead_of_contradicting() {
        let mut search = Search {
            value: "kind:image ext:pdf kind:video ext:.PDF,docx ".into(),
            ..Default::default()
        };
        extract_filters(&mut search);
        assert_eq!(
            search.chips,
            vec![
                Chip::Kind(vec![FileKind::Image, FileKind::Video]),
                Chip::Ext(vec!["pdf".into(), "docx".into()]),
            ]
        );
    }

    #[test]
    fn extension_text_is_forgiving() {
        assert_eq!(parse_extensions(" .PDF, docx  pdf,,"), ["pdf", "docx"]);
    }
}
