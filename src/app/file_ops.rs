//! Connects the background file-operation worker to the UI: submitting
//! operations, progress in the status bar, undo/redo, refreshing affected
//! folders and the dialogs that operations need (rename, confirm, conflicts).

use super::App;
use super::commands::{ActionToPerform, COMMANDS_QUEUE, ModalWindow, TabAction};
use super::ui::{self, DialogButton};
use super::undo::{UndoItem, UndoStack};
use crate::app::dock::TabId;
use crate::clipboard;
use crate::fileops::{
    self, ConflictPolicy, FileOp, FileOpEvent, Origin, Outcome, Worker, display_name,
};
use crate::helper::DataHolder;
use crate::toast;
use egui::{Context, Ui};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Instant;

/// What to do with the items an operation created once it finishes.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum AfterOp {
    #[default]
    Nothing,
    /// Select the created items in the tab that started the operation.
    Select,
    /// Select the single created item and start renaming it.
    SelectAndRename,
}

#[derive(Debug, Clone)]
pub struct RunningOp {
    pub id: u64,
    pub description: String,
    pub fraction: f32,
    pub current: String,
}

#[derive(Debug, Clone)]
struct Submitted {
    tab_id: Option<TabId>,
    after: AfterOp,
    done_message: String,
    /// Paths the operation touches, so ejecting a drive can find it.
    paths: Vec<PathBuf>,
}

/// A finished operation, for the Activity list.
#[derive(Debug, Clone)]
pub struct FinishedOp {
    pub message: String,
    pub errors: Vec<String>,
    pub cancelled: bool,
    pub when: Instant,
}

#[derive(Debug, Default)]
pub struct FileOps {
    worker: Option<Worker>,
    /// Recently finished operations, newest last.
    pub finished: std::collections::VecDeque<FinishedOp>,
    /// File operations and search edits, in one history.
    pub undo: UndoStack,
    pub running: Vec<RunningOp>,
    submitted: HashMap<u64, Submitted>,
}

impl FileOps {
    fn submit(
        &mut self,
        ctx: &Context,
        op: FileOp,
        origin: Origin,
        tab_id: Option<TabId>,
        after: AfterOp,
    ) -> u64 {
        let done_message = op.describe_done();
        let paths = op.paths().into_iter().map(Path::to_path_buf).collect();
        let worker = self.worker.get_or_insert_with(|| {
            let ctx = ctx.clone();
            Worker::new(move |event| {
                COMMANDS_QUEUE.push(ActionToPerform::FileOpEvent(event));
                ctx.request_repaint();
            })
        });
        let id = worker.submit(op, origin);
        self.submitted.insert(
            id,
            Submitted {
                tab_id,
                after,
                done_message,
                paths,
            },
        );
        id
    }

    /// Running operations that touch anything below `mount`.
    pub fn running_on(&self, mount: &Path) -> Vec<u64> {
        self.running
            .iter()
            .map(|op| op.id)
            .filter(|id| {
                self.submitted.get(id).is_some_and(|submitted| {
                    submitted
                        .paths
                        .iter()
                        .any(|path| crate::drives::is_under(path, mount))
                })
            })
            .collect()
    }

    pub fn cancel(&self, id: u64) {
        if let Some(worker) = &self.worker {
            worker.cancel(id);
        }
    }
}

/// State of the rename dialog.
#[derive(Debug, Clone)]
pub struct RenameState {
    path: PathBuf,
    name: String,
    initialized: bool,
}

/// How the batch rename dialog builds new names.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BatchMode {
    /// Replace text in every name.
    Replace,
    /// Build names from a pattern with `{name}`, `{n}` and `{ext}`.
    Pattern,
}

/// State of the batch rename dialog.
#[derive(Debug, Clone)]
pub struct BatchRename {
    paths: Vec<PathBuf>,
    mode: BatchMode,
    find: String,
    replace: String,
    pattern: String,
    start: usize,
    focused: bool,
}

impl BatchRename {
    pub fn new(paths: Vec<PathBuf>) -> Self {
        Self {
            paths,
            mode: BatchMode::Replace,
            find: String::new(),
            replace: String::new(),
            pattern: "{name} ({n}){ext}".to_owned(),
            start: 1,
            focused: false,
        }
    }

    /// The new name for each item, with a problem when it can't be used.
    // `{name}` and friends are the pattern's own placeholders, not format args.
    #[allow(clippy::literal_string_with_formatting_args)]
    pub fn preview(&self) -> Vec<(PathBuf, String, Option<&'static str>)> {
        let width = (self.start + self.paths.len()).to_string().len();
        let names: Vec<String> = self
            .paths
            .iter()
            .enumerate()
            .map(|(index, path)| {
                let name = display_name(path);
                match self.mode {
                    BatchMode::Replace if self.find.is_empty() => name,
                    BatchMode::Replace => name.replace(&self.find, &self.replace),
                    BatchMode::Pattern => {
                        let is_dir = path.is_dir() && !crate::fs_enum::is_package(path);
                        let (stem, ext) = match name.rfind('.') {
                            Some(dot) if dot > 0 && !is_dir => name.split_at(dot),
                            _ => (name.as_str(), ""),
                        };
                        let number = format!("{:0width$}", self.start + index);
                        self.pattern
                            .replace("{name}", stem)
                            .replace("{ext}", ext)
                            .replace("{n}", &number)
                    }
                }
            })
            .collect();
        self.paths
            .iter()
            .zip(&names)
            .map(|(path, name)| {
                let target = path.with_file_name(name);
                let problem = fileops::validate_name(name).err().or_else(|| {
                    if names.iter().filter(|other| *other == name).count() > 1 {
                        Some("Two items would get this name.")
                    } else if target != *path
                        && target.symlink_metadata().is_ok()
                        && !self.paths.contains(&target)
                    {
                        Some("An item with this name already exists.")
                    } else {
                        None
                    }
                });
                (path.clone(), name.clone(), problem)
            })
            .collect()
    }
}

/// A copy or move waiting for the user to decide how to handle conflicts.
#[derive(Debug, Clone)]
pub struct PendingTransfer {
    srcs: Vec<PathBuf>,
    dst: PathBuf,
    moving: bool,
    conflicts: Vec<String>,
}

/// State of the "Move to… / Copy to…" folder prompt.
#[derive(Debug, Clone)]
pub struct ChooseFolder {
    srcs: Vec<PathBuf>,
    moving: bool,
    input: String,
    focused: bool,
}

impl ChooseFolder {
    pub fn new(srcs: Vec<PathBuf>, moving: bool, start: &Path) -> Self {
        Self {
            srcs,
            moving,
            input: start.display().to_string(),
            focused: false,
        }
    }
}

/// Items the user asked to delete permanently, waiting for confirmation.
#[derive(Debug, Clone, Default)]
pub struct PendingDelete {
    pub paths: Vec<PathBuf>,
    /// Delete permanently; otherwise move to the Recycle Bin.
    pub permanent: bool,
    /// Rows of the Recycle Bin view to delete for good, instead of `paths`.
    pub trash: Option<Vec<crate::fileops::trash_bin::TrashKey>>,
    /// Empty the whole Recycle Bin.
    pub empty_trash: bool,
}

/// Items dropped onto a folder, waiting for the user to confirm the move or copy.
#[derive(Debug, Clone)]
pub struct PendingDrop {
    srcs: Vec<PathBuf>,
    dst: PathBuf,
    moving: bool,
}

/// Items to select once the tab's listing contains them.
#[derive(Debug, Clone)]
pub struct PendingSelect {
    pub paths: Vec<PathBuf>,
    pub rename: bool,
    pub since: Instant,
}

impl App {
    /// Paths of the selected items in the active tab, in display order.
    pub(crate) fn selected_paths(&mut self) -> Vec<PathBuf> {
        let Some(tab) = self.tabs.get_current_tab() else {
            return Vec::new();
        };
        tab.selection
            .rows()
            .filter_map(|row| tab.entry_at(row).map(|entry| entry.get_path()))
            .collect()
    }

    /// The item a single-item command (rename) acts on: the focused row if it
    /// is selected, otherwise the first selected row.
    fn primary_selected_path(&mut self) -> Option<PathBuf> {
        let tab = self.tabs.get_current_tab()?;
        let row = tab
            .selection
            .cursor()
            .filter(|&row| tab.selection.contains(row))
            .or_else(|| tab.selection.rows().next())?;
        tab.entry_at(row).map(|entry| entry.get_path())
    }

    /// Puts the selected items on the system clipboard.
    pub(crate) fn copy_selection(&mut self, mode: clipboard::Mode) {
        let paths = self.selected_paths();
        if paths.is_empty() {
            return;
        }
        if let Err(error) = clipboard::write_files(&paths, mode) {
            toast!(Error, "Could not use the clipboard: {error}");
            return;
        }
        self.cut_sequence = clipboard::sequence_number();
        self.tabs.cut = if mode == clipboard::Mode::Cut {
            Arc::new(paths.into_iter().collect())
        } else {
            Arc::default()
        };
    }

    /// Pastes the clipboard's files into `folder`: a copy, or a move when
    /// they were cut.
    pub(crate) fn paste_into(&mut self, ctx: &Context, folder: PathBuf) {
        let Some((paths, mode)) = clipboard::read_files() else {
            toast!(Info, "There are no files on the clipboard.");
            return;
        };
        let moving = mode == clipboard::Mode::Cut;
        if moving {
            self.tabs.cut = Arc::default();
        }
        self.transfer(ctx, paths, folder, moving);
    }

    /// Un-dims cut items once something else replaced them on the clipboard.
    pub(crate) fn forget_stale_cut(&mut self) {
        if !self.tabs.cut.is_empty() && clipboard::sequence_number() != self.cut_sequence {
            self.tabs.cut = Arc::default();
        }
    }

    /// The folder the active tab shows, when it shows exactly one.
    pub(crate) fn current_folder(&mut self) -> Option<PathBuf> {
        self.tabs.get_current_tab()?.current_path.single_path()
    }

    pub(crate) fn submit_file_op(&mut self, ctx: &Context, op: FileOp, after: AfterOp) {
        let tab_id = self.tabs.get_current_tab().map(|tab| tab.id);
        self.file_ops.submit(ctx, op, Origin::User, tab_id, after);
    }

    pub(crate) fn trash_selection(&mut self, ctx: &Context) {
        let paths = self.selected_paths();
        if paths.is_empty() {
            return;
        }
        if self.settings.confirm_trash {
            self.pending_delete = PendingDelete {
                paths,
                permanent: false,
                ..PendingDelete::default()
            };
            self.display_modal = Some(ModalWindow::ConfirmDelete);
        } else {
            self.submit_file_op(ctx, FileOp::Trash(paths), AfterOp::Nothing);
        }
    }

    pub(crate) fn confirm_delete_selection(&mut self) {
        let paths = self.selected_paths();
        if !paths.is_empty() {
            self.pending_delete = PendingDelete {
                paths,
                permanent: true,
                ..PendingDelete::default()
            };
            self.display_modal = Some(ModalWindow::ConfirmDelete);
        }
    }

    /// F2: rename the item, or open batch rename for several items.
    pub(crate) fn rename_selection(&mut self) {
        let paths = self.selected_paths();
        if paths.len() > 1 {
            self.batch_rename = Some(BatchRename::new(paths));
            self.display_modal = Some(ModalWindow::BatchRename);
        } else if let Some(path) = self.primary_selected_path() {
            self.begin_rename(path);
        }
    }

    /// Draws the dialog for `ModalWindow::BatchRename`.
    #[allow(clippy::too_many_lines)]
    pub(crate) fn batch_rename_ui(&mut self, ctx: &Context) {
        let Some(state) = &mut self.batch_rename else {
            ActionToPerform::CloseActiveModalWindow.schedule();
            return;
        };
        let mut confirm = false;
        let mut cancel = false;
        let preview = state.preview();
        let changes = preview
            .iter()
            .filter(|(path, name, _)| display_name(path) != *name)
            .count();
        let problems = preview.iter().filter(|(_, _, p)| p.is_some()).count();
        let modal = egui::Modal::new(egui::Id::new(ModalWindow::BatchRename))
            .frame(ui::dialog_frame(ctx))
            .show(ctx, |ui| {
                ui.set_width((ctx.content_rect().width() - 64.0).clamp(320.0, 620.0));
                ui.heading(format!("Rename {} items", state.paths.len()));
                ui.horizontal(|ui| {
                    ui.selectable_value(&mut state.mode, BatchMode::Replace, "Find and replace");
                    ui.selectable_value(&mut state.mode, BatchMode::Pattern, "Name pattern");
                });
                match state.mode {
                    BatchMode::Replace => {
                        egui::Grid::new("batch_replace")
                            .num_columns(2)
                            .show(ui, |ui| {
                                ui.label("Find");
                                let find = ui.add(ui::text_field(ui, &mut state.find));
                                if !state.focused {
                                    state.focused = true;
                                    find.request_focus();
                                }
                                ui.end_row();
                                ui.label("Replace with");
                                ui.add(ui::text_field(ui, &mut state.replace));
                                ui.end_row();
                            });
                    }
                    BatchMode::Pattern => {
                        egui::Grid::new("batch_pattern")
                            .num_columns(2)
                            .show(ui, |ui| {
                                ui.label("Pattern");
                                ui.add(ui::text_field(ui, &mut state.pattern));
                                ui.end_row();
                                ui.label("Start at");
                                ui.add(egui::DragValue::new(&mut state.start).range(0..=100_000));
                                ui.end_row();
                            });
                        ui.weak("{name} is the old name, {ext} its extension, {n} a number.");
                    }
                }
                ui.separator();
                egui::ScrollArea::vertical()
                    .max_height(260.0)
                    .show(ui, |ui| {
                        egui::Grid::new("batch_preview")
                            .num_columns(3)
                            .striped(true)
                            .show(ui, |ui| {
                                for (path, name, problem) in &preview {
                                    ui.label(display_name(path));
                                    ui.label("→");
                                    match problem {
                                        Some(problem) => {
                                            ui.colored_label(ui.visuals().error_fg_color, name)
                                                .on_hover_text(*problem);
                                        }
                                        None => {
                                            ui.strong(name);
                                        }
                                    }
                                    ui.end_row();
                                }
                            });
                    });
                ui.separator();
                if problems > 0 {
                    ui.colored_label(
                        ui.visuals().error_fg_color,
                        format!("{problems} names can't be used; hover them for details."),
                    );
                }
                let rename = format!("Rename {changes} items");
                match ui::dialog_buttons(
                    ui,
                    false,
                    &[
                        DialogButton::primary(&rename).enabled(problems == 0 && changes > 0),
                        DialogButton::plain("Cancel"),
                    ],
                ) {
                    Some(0) => confirm = true,
                    Some(_) => cancel = true,
                    None => {}
                }
            });
        if confirm {
            let pairs: Vec<(PathBuf, PathBuf)> = preview
                .into_iter()
                .filter(|(path, name, _)| display_name(path) != *name)
                .map(|(path, name, _)| {
                    let target = path.with_file_name(&name);
                    (path, target)
                })
                .collect();
            self.batch_rename = None;
            self.submit_file_op(ctx, FileOp::RenameMany(pairs), AfterOp::Select);
            ActionToPerform::CloseActiveModalWindow.schedule();
        } else if cancel || modal.should_close() || escape_pressed(ctx) {
            self.batch_rename = None;
            ActionToPerform::CloseActiveModalWindow.schedule();
        }
    }

    pub(crate) fn begin_rename(&mut self, path: PathBuf) {
        self.rename = Some(RenameState {
            name: display_name(&path),
            path,
            initialized: false,
        });
        self.display_modal = Some(ModalWindow::Rename);
    }

    pub(crate) fn new_item(&mut self, ctx: &Context, folder: bool) {
        let Some(parent) = self.current_folder() else {
            toast!(Info, "Open a single folder to create items in it.");
            return;
        };
        let op = if folder {
            FileOp::NewFolder { parent }
        } else {
            FileOp::NewFile { parent }
        };
        self.submit_file_op(ctx, op, AfterOp::SelectAndRename);
    }

    pub(crate) fn duplicate_selection(&mut self, ctx: &Context) {
        let paths = self.selected_paths();
        if !paths.is_empty() {
            self.submit_file_op(ctx, FileOp::Duplicate(paths), AfterOp::Select);
        }
    }

    /// Copies or moves `srcs` into `dst`, asking first when names collide.
    pub(crate) fn transfer(
        &mut self,
        ctx: &Context,
        srcs: Vec<PathBuf>,
        dst: PathBuf,
        moving: bool,
    ) {
        if srcs.is_empty() {
            return;
        }
        let conflicts: Vec<String> = srcs
            .iter()
            .filter(|src| src.parent() != Some(dst.as_path()))
            .filter_map(|src| src.file_name())
            .filter(|name| dst.join(name).symlink_metadata().is_ok())
            .map(|name| name.to_string_lossy().into_owned())
            .collect();
        if conflicts.is_empty() {
            self.submit_transfer(ctx, srcs, dst, moving, ConflictPolicy::KeepBoth);
        } else {
            self.pending_transfer = Some(PendingTransfer {
                srcs,
                dst,
                moving,
                conflicts,
            });
            self.display_modal = Some(ModalWindow::Conflict);
        }
    }

    /// Items dropped onto a folder: asks first, unless turned off in settings.
    pub(crate) fn drop_transfer(
        &mut self,
        ctx: &Context,
        srcs: Vec<PathBuf>,
        dst: PathBuf,
        moving: bool,
    ) {
        if self.settings.confirm_drop {
            self.pending_drop = Some(PendingDrop { srcs, dst, moving });
            self.display_modal = Some(ModalWindow::ConfirmDrop);
        } else {
            self.transfer(ctx, srcs, dst, moving);
        }
    }

    /// Items dropped onto the Recycle Bin: asks first, like a drop on a folder.
    pub(crate) fn drop_to_trash(&mut self, ctx: &Context, paths: Vec<PathBuf>) {
        if paths.is_empty() {
            return;
        }
        if self.settings.confirm_drop || self.settings.confirm_trash {
            self.pending_delete = PendingDelete {
                paths,
                permanent: false,
                ..PendingDelete::default()
            };
            self.display_modal = Some(ModalWindow::ConfirmDelete);
        } else {
            self.submit_file_op(ctx, FileOp::Trash(paths), AfterOp::Nothing);
        }
    }

    fn submit_transfer(
        &mut self,
        ctx: &Context,
        srcs: Vec<PathBuf>,
        dst: PathBuf,
        moving: bool,
        policy: ConflictPolicy,
    ) {
        let op = if moving {
            FileOp::Move { srcs, dst, policy }
        } else {
            FileOp::Copy { srcs, dst, policy }
        };
        self.submit_file_op(ctx, op, AfterOp::Select);
    }

    /// Ctrl+Z / Ctrl+Y: reverts (or re-applies) the latest change, whether it
    /// was a file operation, a search edit or a change to saved searches.
    pub(crate) fn undo(&mut self, ctx: &Context, redo: bool) {
        let (item, origin) = if redo {
            (self.file_ops.undo.pop_redo(), Origin::Redo)
        } else {
            (self.file_ops.undo.pop_undo(), Origin::Undo)
        };
        let Some(item) = item else {
            toast!(Info, "Nothing to {}.", if redo { "redo" } else { "undo" });
            return;
        };
        let verb = if redo { "Redid" } else { "Undid" };
        match item {
            UndoItem::File(undo) => {
                let tab_id = self.tabs.get_current_tab().map(|tab| tab.id);
                self.file_ops
                    .submit(ctx, FileOp::Undo(undo), origin, tab_id, AfterOp::Select);
            }
            UndoItem::Search {
                tab,
                ref before,
                ref after,
                ref label,
            } => {
                let state = if redo { after.clone() } else { before.clone() };
                toast!(Info, "{verb} {label}");
                self.restore_search(tab, state);
                self.file_ops.undo.record(origin, item);
            }
            UndoItem::SavedSearches {
                ref before,
                ref after,
                ref label,
            } => {
                let list = if redo { after.clone() } else { before.clone() };
                toast!(Info, "{verb} {label}");
                ctx.data_set_persisted(super::SavedSearches { searches: list });
                self.file_ops.undo.record(origin, item);
            }
        }
    }

    pub(crate) fn handle_file_op_event(&mut self, event: FileOpEvent) {
        match event {
            FileOpEvent::Started { id, description } => {
                self.file_ops.running.push(RunningOp {
                    id,
                    description,
                    fraction: 0.0,
                    current: String::new(),
                });
            }
            FileOpEvent::Progress {
                id,
                fraction,
                current,
            } => {
                if let Some(op) = self.file_ops.running.iter_mut().find(|op| op.id == id) {
                    op.fraction = fraction;
                    op.current = current;
                }
            }
            FileOpEvent::Finished(outcome) => self.finish_file_op(outcome),
        }
    }

    fn finish_file_op(&mut self, outcome: Outcome) {
        self.file_ops.running.retain(|op| op.id != outcome.id);
        let submitted = self.file_ops.submitted.remove(&outcome.id);
        self.file_ops.finished.push_back(FinishedOp {
            message: submitted
                .as_ref()
                .map_or_else(|| outcome.description.clone(), |s| s.done_message.clone()),
            errors: outcome.errors.clone(),
            cancelled: outcome.cancelled,
            when: Instant::now(),
        });
        if self.file_ops.finished.len() > 30 {
            self.file_ops.finished.pop_front();
        }
        match &outcome.undo {
            Some(crate::fileops::Undo::Rename { from, to }) => {
                crate::app::open_history::rename(from, to);
                crate::app::thumbnail_choice::rename(from, to);
            }
            Some(crate::fileops::Undo::Moves(pairs)) => {
                for (now, was) in pairs {
                    crate::app::open_history::rename(was, now);
                    crate::app::thumbnail_choice::rename(was, now);
                }
            }
            _ => {}
        }
        if let Some(undo) = outcome.undo.clone() {
            self.file_ops
                .undo
                .record(outcome.origin, UndoItem::File(undo));
        }
        crate::app::database::invalidate_dirs(outcome.affected_dirs.iter().cloned());
        self.refresh_tabs_showing(&outcome.affected_dirs, outcome.trash_changed);
        self.plugins.folders_changed(&outcome.affected_dirs);
        if self.plugins.wants("file_op_finished") {
            let paths = |paths: &[std::path::PathBuf]| -> Vec<String> {
                paths
                    .iter()
                    .map(|path| path.to_string_lossy().into_owned())
                    .collect()
            };
            self.plugins.emit(
                "file_op_finished",
                &crate::plugins::CallContext::default(),
                &serde_json::json!({
                    "description": outcome.description,
                    "errors": outcome.errors,
                    "cancelled": outcome.cancelled,
                    "affected_dirs": paths(&outcome.affected_dirs),
                    "created": paths(&outcome.created),
                }),
            );
        }

        if let Some(first) = outcome.errors.first() {
            let more = outcome.errors.len() - 1;
            if more == 0 {
                toast!(Error, "{first}");
            } else {
                toast!(Error, "{first} (and {more} more)");
            }
        } else if outcome.cancelled {
            toast!(Info, "Cancelled: {}", outcome.description);
        } else if let Some(submitted) = &submitted
            && matches!(outcome.origin, Origin::User)
            && outcome.undo.is_some()
            && submitted.after != AfterOp::SelectAndRename
        {
            toast!(Success, "{} · Ctrl+Z to undo", submitted.done_message);
        }

        let Some(submitted) = submitted else {
            return;
        };
        if submitted.after == AfterOp::Nothing || outcome.created.is_empty() {
            return;
        }
        let tab = match submitted.tab_id {
            Some(id) => self.tabs.get_tab_by_id(id),
            None => self.tabs.get_current_tab(),
        };
        if let Some(tab) = tab {
            tab.pending_select = Some(PendingSelect {
                paths: outcome.created,
                rename: submitted.after == AfterOp::SelectAndRename,
                since: Instant::now(),
            });
        }
    }

    /// Reloads every tab whose folders include one of `dirs`.
    fn refresh_tabs_showing(&self, dirs: &[PathBuf], trash_changed: bool) {
        let ids: Vec<TabId> = self
            .tabs
            .tabs()
            .filter(|tab| {
                tab.current_path
                    .get_paths()
                    .iter()
                    .any(|shown| dirs.iter().any(|dir| same_dir(shown, dir)))
                    || tab.search_depth() > 1
                    || (trash_changed && tab.current_path.is_trash())
            })
            .map(|tab| tab.id)
            .collect();
        for id in ids {
            TabAction::RequestFilesRefresh.schedule_tab(id);
        }
    }

    /// Copies files dropped from other applications into the open folder and
    /// shows where they will land while they are dragged over the window.
    pub(crate) fn handle_external_drop(&mut self, ctx: &Context) {
        let (dropped, hovering) = ctx.input(|i| {
            (
                i.raw
                    .dropped_files
                    .iter()
                    .map(|file| file.path().to_path_buf())
                    .filter(|path| !path.as_os_str().is_empty())
                    .collect::<Vec<_>>(),
                !i.raw.hovered_files.is_empty(),
            )
        });
        let folder = self.current_folder();
        if hovering {
            let text = folder.as_ref().map_or_else(
                || "Open a single folder to drop files into it".to_owned(),
                |folder| format!("Drop to copy into {}", display_name(folder)),
            );
            let rect = ctx.content_rect();
            let painter = ctx.layer_painter(egui::LayerId::new(
                egui::Order::Foreground,
                egui::Id::new("external_drop"),
            ));
            let accent = ui::Palette::for_context(ctx, ctx.global_style().visuals.dark_mode).accent;
            painter.rect_filled(rect, 0.0, accent.gamma_multiply(0.12));
            painter.rect_stroke(
                rect.shrink(4.0),
                8.0,
                egui::Stroke::new(2.0, accent),
                egui::StrokeKind::Inside,
            );
            painter.text(
                rect.center(),
                egui::Align2::CENTER_CENTER,
                text,
                egui::FontId::proportional(18.0),
                ctx.global_style().visuals.strong_text_color(),
            );
        }
        if dropped.is_empty() {
            return;
        }
        if let Some(folder) = folder {
            self.transfer(ctx, dropped, folder, false);
        } else {
            toast!(Info, "Open a single folder to drop files into it.");
        }
    }

    /// Hands an in-app drag to the operating system once it leaves the
    /// window, so other applications (a video player, a browser) can take
    /// the files.
    pub(crate) fn hand_off_external_drag(&self, ctx: &Context) {
        if !crate::native_drag::SUPPORTED || self.display_modal.is_some() {
            return;
        }
        let Some(payload) = egui::DragAndDrop::payload::<DraggedPaths>(ctx) else {
            return;
        };
        let rect = ctx.content_rect();
        let left_window = ctx.input(|i| {
            i.pointer.primary_down()
                && (i.pointer.hover_pos().is_none()
                    || i.pointer
                        .latest_pos()
                        .is_some_and(|pos| !rect.contains(pos)))
        });
        if !left_window {
            return;
        }
        egui::DragAndDrop::clear_payload(ctx);
        let paths: Vec<PathBuf> = payload.0.iter().filter(|p| p.exists()).cloned().collect();
        if let Err(error) = crate::native_drag::start(&paths) {
            toast!(Error, "Could not drag the files: {error}");
        }
        ctx.request_repaint();
    }

    /// A small label next to the pointer while items are dragged in the app.
    pub(crate) fn drag_badge(ctx: &Context) {
        let Some(payload) = egui::DragAndDrop::payload::<DraggedPaths>(ctx) else {
            return;
        };
        let Some(pos) = ctx.pointer_hover_pos() else {
            return;
        };
        let modifiers = ctx.input(|i| i.modifiers);
        let verb = if modifiers.command {
            "Copy"
        } else if modifiers.shift {
            "Move"
        } else {
            "Drag"
        };
        let what = match payload.0.as_slice() {
            [one] => display_name(one),
            many => format!("{} items", many.len()),
        };
        egui::Area::new(egui::Id::new("drag_badge"))
            .fixed_pos(pos + egui::vec2(16.0, 16.0))
            .order(egui::Order::Tooltip)
            .interactable(false)
            .show(ctx, |ui| {
                egui::Frame::popup(ui.style()).show(ui, |ui| {
                    ui.label(format!("{verb} {what}"));
                });
            });
    }

    /// Status-bar indicator for running operations, with progress and Cancel.
    pub(crate) fn file_ops_status(&self, ui: &mut Ui) {
        if !self.file_ops.finished.is_empty() {
            ui.separator();
            let response = ui::icon_button(ui, lucide_icons::Icon::History, "Activity", false);
            egui::Popup::menu(&response).show(|ui| {
                ui.set_min_width(320.0);
                ui.strong("Recent activity");
                for op in self.file_ops.finished.iter().rev() {
                    ui.horizontal(|ui| {
                        let (mark, color) = if !op.errors.is_empty() {
                            ("✗", ui.visuals().error_fg_color)
                        } else if op.cancelled {
                            ("–", ui.visuals().weak_text_color())
                        } else {
                            ("✓", ui::Palette::of(ui).accent)
                        };
                        ui.colored_label(color, mark);
                        let label = ui.label(&op.message);
                        if !op.errors.is_empty() {
                            label.on_hover_text(op.errors.join("\n"));
                        }
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            ui.weak(ago(op.when));
                        });
                    });
                }
                if let Some(next) = self.file_ops.undo.peek_undo() {
                    ui.separator();
                    if ui
                        .button(format!("Undo {} (Ctrl+Z)", next.label()))
                        .clicked()
                    {
                        ActionToPerform::RunCommand(super::keymap::Command::Undo).schedule();
                        ui.close();
                    }
                }
            });
        }
        for op in &self.file_ops.running {
            ui.separator();
            ui.add(
                egui::ProgressBar::new(op.fraction)
                    .desired_width(140.0)
                    .show_percentage(),
            )
            .on_hover_text(if op.current.is_empty() {
                op.description.clone()
            } else {
                format!("{}\n{}", op.description, op.current)
            });
            ui.weak(&op.description);
            if ui::icon_button(ui, lucide_icons::Icon::X, "Cancel", false).clicked() {
                self.file_ops.cancel(op.id);
            }
        }
    }

    /// Draws the dialog for `ModalWindow::Rename`.
    pub(crate) fn rename_ui(&mut self, ctx: &Context) {
        let Some(state) = &mut self.rename else {
            ActionToPerform::CloseActiveModalWindow.schedule();
            return;
        };
        let mut commit = false;
        let mut cancel = false;
        let original = display_name(&state.path);
        let error = rename_error(&state.path, &original, &state.name);
        let modal = egui::Modal::new(egui::Id::new(ModalWindow::Rename))
            .frame(ui::dialog_frame(ctx))
            .show(ctx, |ui| {
                ui.set_width((ctx.content_rect().width() - 64.0).clamp(280.0, 420.0));
                ui.heading("Rename");
                ui.label("New name");
                let id = egui::Id::new("rename_field");
                let response = ui.add(
                    ui::text_field(ui, &mut state.name)
                        .id(id)
                        .desired_width(ui.available_width()),
                );
                if !state.initialized {
                    state.initialized = true;
                    response.request_focus();
                    select_stem(
                        ctx,
                        id,
                        &state.name,
                        state.path.is_dir() && !crate::fs_enum::is_package(&state.path),
                    );
                }
                if response.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                    commit = true;
                }
                if let Some(error) = error {
                    ui.colored_label(ui.visuals().error_fg_color, error);
                }
                let can_rename = error.is_none() && state.name != original;
                match ui::dialog_buttons(
                    ui,
                    false,
                    &[
                        DialogButton::primary("Rename").enabled(can_rename),
                        DialogButton::plain("Cancel"),
                    ],
                ) {
                    Some(0) => commit = true,
                    Some(_) => cancel = true,
                    None => {}
                }
            });
        let unchanged = state.name == original;
        if commit && (error.is_some() || unchanged) {
            // Enter on an unchanged name just closes; on an invalid one, stay.
            commit = false;
            cancel = unchanged;
        }
        if commit {
            let from = state.path.clone();
            let to = from.with_file_name(&state.name);
            self.rename = None;
            self.submit_file_op(ctx, FileOp::Rename { from, to }, AfterOp::Select);
            ActionToPerform::CloseActiveModalWindow.schedule();
        } else if cancel || modal.should_close() || escape_pressed(ctx) {
            self.rename = None;
            ActionToPerform::CloseActiveModalWindow.schedule();
        }
    }

    /// Draws the dialog for `ModalWindow::ConfirmDelete`.
    pub(crate) fn confirm_delete_ui(&mut self, ctx: &Context) {
        let mut confirmed = false;
        let mut cancel = false;
        let paths = &self.pending_delete.paths;
        let permanent = self.pending_delete.permanent;
        let empty_trash = self.pending_delete.empty_trash;
        let modal = egui::Modal::new(egui::Id::new(ModalWindow::ConfirmDelete))
            .frame(ui::dialog_frame(ctx))
            .show(ctx, |ui| {
                ui.set_width((ctx.content_rect().width() - 64.0).clamp(280.0, 440.0));
                let bin = crate::fileops::trash_bin::LABEL;
                if empty_trash {
                    ui.heading(format!("Empty the {bin}?"));
                } else if permanent {
                    ui.heading("Delete permanently?");
                } else {
                    ui.heading(format!("Move to the {bin}?"));
                }
                let what = match paths.as_slice() {
                    [one] => format!("\"{}\"", display_name(one)),
                    many => format!("{} items", many.len()),
                };
                ui.label(if empty_trash {
                    format!("Everything in the {bin} will be deleted. This can't be undone.")
                } else if permanent {
                    format!("{what} will be deleted. This can't be undone.")
                } else {
                    format!("{what} will be moved to the {bin}. Ctrl+Z brings it back.")
                });
                if paths.len() > 1 {
                    egui::ScrollArea::vertical()
                        .max_height(140.0)
                        .show(ui, |ui| {
                            for path in paths {
                                ui.weak(display_name(path));
                            }
                        });
                }
                // The destructive button is the default, as in Explorer.
                let move_to_bin = format!("Move to the {bin}");
                let action = if empty_trash {
                    DialogButton::danger("Empty")
                } else if permanent {
                    DialogButton::danger("Delete")
                } else {
                    DialogButton::primary(&move_to_bin)
                };
                match ui::dialog_buttons(ui, true, &[action, DialogButton::plain("Cancel")]) {
                    Some(0) => confirmed = true,
                    Some(_) => cancel = true,
                    None => {}
                }
            });
        if confirmed {
            let pending = std::mem::take(&mut self.pending_delete);
            let op = if pending.empty_trash {
                FileOp::EmptyTrash
            } else if let Some(keys) = pending.trash {
                FileOp::PurgeTrashed(keys)
            } else if pending.permanent {
                FileOp::Delete(pending.paths)
            } else {
                FileOp::Trash(pending.paths)
            };
            self.submit_file_op(ctx, op, AfterOp::Nothing);
            ActionToPerform::CloseActiveModalWindow.schedule();
        } else if cancel || modal.should_close() || escape_pressed(ctx) {
            self.pending_delete = PendingDelete::default();
            ActionToPerform::CloseActiveModalWindow.schedule();
        }
    }

    /// Draws the dialog for `ModalWindow::ConfirmDrop`: items were dropped
    /// onto a folder, and are moved or copied only once confirmed.
    pub(crate) fn confirm_drop_ui(&mut self, ctx: &Context) {
        let Some(pending) = &self.pending_drop else {
            ActionToPerform::CloseActiveModalWindow.schedule();
            return;
        };
        let mut choice = None;
        let mut cancel = false;
        let (verb, other) = if pending.moving {
            ("Move", "Copy")
        } else {
            ("Copy", "Move")
        };
        let modal = egui::Modal::new(egui::Id::new(ModalWindow::ConfirmDrop))
            .frame(ui::dialog_frame(ctx))
            .show(ctx, |ui| {
                ui.set_width((ctx.content_rect().width() - 64.0).clamp(300.0, 460.0));
                let what = match pending.srcs.as_slice() {
                    [one] => format!("\"{}\"", display_name(one)),
                    many => format!("{} items", many.len()),
                };
                ui.heading(format!(
                    "{verb} {what} to \"{}\"?",
                    display_name(&pending.dst)
                ));
                ui.add_space(4.0);
                ui.weak(pending.dst.display().to_string());
                if pending.srcs.len() > 1 {
                    ui.add_space(4.0);
                    egui::ScrollArea::vertical()
                        .max_height(140.0)
                        .show(ui, |ui| {
                            for path in &pending.srcs {
                                ui.label(display_name(path));
                            }
                        });
                }
                match ui::dialog_buttons(
                    ui,
                    true,
                    &[
                        DialogButton::primary(verb),
                        DialogButton::plain(other),
                        DialogButton::plain("Cancel"),
                    ],
                ) {
                    Some(0) => choice = Some(pending.moving),
                    Some(1) => choice = Some(!pending.moving),
                    Some(_) => cancel = true,
                    None => {}
                }
            });
        if let Some(moving) = choice
            && let Some(pending) = self.pending_drop.take()
        {
            self.transfer(ctx, pending.srcs, pending.dst, moving);
            // Name conflicts open their own dialog in place of this one.
            if self.display_modal == Some(ModalWindow::ConfirmDrop) {
                ActionToPerform::CloseActiveModalWindow.schedule();
            }
        } else if cancel || modal.should_close() || escape_pressed(ctx) {
            self.pending_drop = None;
            ActionToPerform::CloseActiveModalWindow.schedule();
        }
    }

    /// Draws the dialog for `ModalWindow::ChooseFolder`.
    pub(crate) fn choose_folder_ui(&mut self, ctx: &Context) {
        let Some(state) = &mut self.choose_folder else {
            ActionToPerform::CloseActiveModalWindow.schedule();
            return;
        };
        let mut confirm = false;
        let mut cancel = false;
        let target = PathBuf::from(state.input.trim());
        let valid = target.is_dir() && !crate::fs_enum::is_package(&target);
        let verb = if state.moving { "Move" } else { "Copy" };
        let modal = egui::Modal::new(egui::Id::new(ModalWindow::ChooseFolder))
            .frame(ui::dialog_frame(ctx))
            .show(ctx, |ui| {
                ui.set_width((ctx.content_rect().width() - 64.0).clamp(300.0, 520.0));
                ui.heading(format!(
                    "{verb} {}",
                    match state.srcs.as_slice() {
                        [one] => format!("\"{}\"", display_name(one)),
                        many => format!("{} items", many.len()),
                    }
                ));
                ui.label("Destination folder");
                let response = ui
                    .add(ui::text_field(ui, &mut state.input).desired_width(ui.available_width()));
                if !state.focused {
                    state.focused = true;
                    response.request_focus();
                }
                if response.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                    confirm = true;
                }
                if !valid {
                    ui.colored_label(ui.visuals().error_fg_color, "This folder doesn't exist.");
                }
                match ui::dialog_buttons(
                    ui,
                    false,
                    &[
                        DialogButton::primary(verb).enabled(valid),
                        DialogButton::plain("Cancel"),
                    ],
                ) {
                    Some(0) => confirm = true,
                    Some(_) => cancel = true,
                    None => {}
                }
            });
        if confirm && valid {
            if let Some(state) = self.choose_folder.take() {
                self.transfer(ctx, state.srcs, target, state.moving);
            }
            if self.display_modal == Some(ModalWindow::ChooseFolder) {
                ActionToPerform::CloseActiveModalWindow.schedule();
            }
        } else if cancel || modal.should_close() || escape_pressed(ctx) {
            self.choose_folder = None;
            ActionToPerform::CloseActiveModalWindow.schedule();
        }
    }

    /// Draws the dialog for `ModalWindow::Conflict`.
    pub(crate) fn conflict_ui(&mut self, ctx: &Context) {
        let Some(pending) = &self.pending_transfer else {
            ActionToPerform::CloseActiveModalWindow.schedule();
            return;
        };
        let mut choice = None;
        let mut cancel = false;
        let modal = egui::Modal::new(egui::Id::new(ModalWindow::Conflict))
            .frame(ui::dialog_frame(ctx))
            .show(ctx, |ui| {
                ui.set_width((ctx.content_rect().width() - 64.0).clamp(300.0, 460.0));
                ui.heading("Replace or skip items");
                ui.label(match pending.conflicts.as_slice() {
                    [one] => format!(
                        "The destination \"{}\" already has an item named \"{one}\".",
                        display_name(&pending.dst)
                    ),
                    many => format!(
                        "The destination \"{}\" already has {} items with the same names.",
                        display_name(&pending.dst),
                        many.len()
                    ),
                });
                ui.add_space(12.0);
                let size = [ui.available_width(), ui::control_height(ui)];
                for (label, policy) in [
                    (
                        "Replace the items in the destination",
                        ConflictPolicy::Replace,
                    ),
                    ("Skip these items", ConflictPolicy::Skip),
                    ("Keep both (rename the new items)", ConflictPolicy::KeepBoth),
                ] {
                    if ui.add_sized(size, egui::Button::new(label)).clicked() {
                        choice = Some(policy);
                    }
                }
                cancel = ui::dialog_buttons(ui, false, &[DialogButton::plain("Cancel")]).is_some();
            });
        if let Some(policy) = choice
            && let Some(pending) = self.pending_transfer.take()
        {
            self.submit_transfer(ctx, pending.srcs, pending.dst, pending.moving, policy);
            ActionToPerform::CloseActiveModalWindow.schedule();
        } else if cancel || modal.should_close() || escape_pressed(ctx) {
            self.pending_transfer = None;
            ActionToPerform::CloseActiveModalWindow.schedule();
        }
    }
}

/// Items being dragged inside the app (egui drag-and-drop payload).
#[derive(Debug, Clone)]
pub struct DraggedPaths(pub Vec<PathBuf>);

/// Whether dropping `srcs` onto `dst` moves them (otherwise copies). Like
/// Explorer: Ctrl copies, Shift moves, and without modifiers items move
/// within a drive and are copied to another drive.
pub fn drop_moves(srcs: &[PathBuf], dst: &Path, modifiers: egui::Modifiers) -> bool {
    if modifiers.command {
        return false;
    }
    if modifiers.shift {
        return true;
    }
    srcs.iter()
        .all(|src| src.components().next() == dst.components().next())
}

/// Makes `response` accept dragged items into the folder `dst`: highlights it
/// while hovered and starts the copy or move on release.
pub fn folder_drop_target(ui: &Ui, response: &egui::Response, dst: &Path) {
    let Some(hovered) = response.dnd_hover_payload::<DraggedPaths>() else {
        return;
    };
    // A folder can't take itself, its own subfolders, or items already in it.
    let pointless = hovered.0.iter().all(|src| src.parent() == Some(dst))
        || hovered.0.iter().any(|src| dst.starts_with(src));
    if pointless {
        return;
    }
    ui.painter().rect_stroke(
        response.rect,
        ui::control_radius(ui),
        egui::Stroke::new(2.0, ui::Palette::of(ui).accent),
        egui::StrokeKind::Inside,
    );
    if let Some(payload) = response.dnd_release_payload::<DraggedPaths>() {
        let moving = drop_moves(&payload.0, dst, ui.input(|i| i.modifiers));
        ActionToPerform::DropTransfer {
            srcs: payload.0.clone(),
            dst: dst.to_path_buf(),
            moving,
        }
        .schedule();
    }
}

/// Makes `response` accept dragged items onto the application `app`: they
/// are opened with it, as Finder does, instead of moved into the bundle.
#[cfg(target_os = "macos")]
pub fn app_drop_target(ui: &Ui, response: &egui::Response, app: &Path) {
    let Some(hovered) = response.dnd_hover_payload::<DraggedPaths>() else {
        return;
    };
    if hovered.0.iter().any(|src| src == app) {
        return;
    }
    ui.painter().rect_stroke(
        response.rect,
        ui::control_radius(ui),
        egui::Stroke::new(2.0, ui::Palette::of(ui).accent),
        egui::StrokeKind::Inside,
    );
    if let Some(payload) = response.dnd_release_payload::<DraggedPaths>() {
        crate::shell::open_files_with(app, &payload.0);
        for path in payload.0.iter() {
            crate::app::open_history::opened(path);
        }
    }
}

/// Esc closes a dialog even while one of its text fields has focus.
pub fn escape_pressed(ctx: &Context) -> bool {
    ctx.input(|i| i.key_pressed(egui::Key::Escape))
}

/// "just now", "5 min ago", "2 h ago".
fn ago(when: Instant) -> String {
    let secs = when.elapsed().as_secs();
    match secs {
        0..=59 => "just now".to_owned(),
        60..=3599 => format!("{} min ago", secs / 60),
        _ => format!("{} h ago", secs / 3600),
    }
}

/// Paths in the same form as Explorer's "Copy as path": one quoted path per
/// line, or a single unquoted path.
pub fn paths_as_text(paths: &[PathBuf]) -> String {
    match paths {
        [one] => one.display().to_string(),
        many => many
            .iter()
            .map(|path| format!("\"{}\"", path.display()))
            .collect::<Vec<_>>()
            .join("\r\n"),
    }
}

/// Equal paths, ignoring case where the disks usually do (Windows, macOS).
fn same_dir(a: &Path, b: &Path) -> bool {
    a == b
        || (cfg!(any(windows, target_os = "macos"))
            && a.to_string_lossy().to_lowercase() == b.to_string_lossy().to_lowercase())
}

/// Why `name` can't be used for renaming `path`, if it can't.
fn rename_error(path: &Path, original: &str, name: &str) -> Option<&'static str> {
    if name == original {
        return None;
    }
    if let Err(error) = fileops::validate_name(name) {
        return Some(error);
    }
    let target = path.with_file_name(name);
    if target.symlink_metadata().is_ok() && !fileops::is_respelling(path, &target) {
        return Some("An item with this name already exists.");
    }
    None
}

/// Selects the name without its extension, so typing replaces just the stem.
fn select_stem(ctx: &Context, id: egui::Id, name: &str, is_dir: bool) {
    use egui::text::{CCursor, CCursorRange};
    let stem_chars = match name.rfind('.') {
        Some(dot) if dot > 0 && !is_dir => name[..dot].chars().count(),
        _ => name.chars().count(),
    };
    let mut state = egui::TextEdit::load_state(ctx, id).unwrap_or_default();
    state.cursor.set_char_range(Some(CCursorRange::two(
        CCursor::new(0),
        CCursor::new(stem_chars),
    )));
    egui::TextEdit::store_state(ctx, id, state);
}

#[cfg(test)]
mod tests {
    use super::{BatchMode, BatchRename, drop_moves, paths_as_text};

    #[test]
    fn batch_rename_previews_patterns_and_problems() {
        let mut batch = BatchRename::new(vec![
            PathBuf::from("/nowhere/a.jpg"),
            PathBuf::from("/nowhere/b.jpg"),
        ]);
        batch.mode = BatchMode::Pattern;
        batch.pattern = "Trip {n}{ext}".into();
        let names: Vec<String> = batch.preview().into_iter().map(|(_, n, _)| n).collect();
        assert_eq!(names, vec!["Trip 1.jpg", "Trip 2.jpg"]);
        batch.pattern = "same".into();
        assert!(
            batch
                .preview()
                .iter()
                .all(|(_, _, problem)| problem.is_some())
        );
        batch.mode = BatchMode::Replace;
        batch.find = ".jpg".into();
        batch.replace = ".jpeg".into();
        assert_eq!(batch.preview()[0].1, "a.jpeg");
    }

    use egui::Modifiers;
    use std::path::{Path, PathBuf};

    #[test]
    fn drops_move_within_a_drive_and_copy_across() {
        let src = [PathBuf::from("/data/a.txt")];
        assert!(drop_moves(&src, Path::new("/data/b"), Modifiers::NONE));
        assert!(!drop_moves(&src, Path::new("/data/b"), Modifiers::COMMAND));
        if cfg!(windows) {
            let src = [PathBuf::from(r"C:\data\a.txt")];
            assert!(!drop_moves(&src, Path::new(r"D:\backup"), Modifiers::NONE));
            assert!(drop_moves(&src, Path::new(r"D:\backup"), Modifiers::SHIFT));
        }
    }

    #[test]
    fn copy_as_path_quotes_multiple_paths() {
        assert_eq!(paths_as_text(&[PathBuf::from("a b")]), "a b");
        assert_eq!(
            paths_as_text(&[PathBuf::from("a"), PathBuf::from("b")]),
            "\"a\"\r\n\"b\""
        );
    }
}
