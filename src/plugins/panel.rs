//! The results dialog of an action with `results = true`: its options, the
//! buttons to run and stop it, and the table its job fills in. Hiding it
//! leaves the job running; the status bar and the action itself bring it back.

use super::CallContext;
use super::form::FormState;
use super::table::{RowClick, SizeFilter, SortOrder, TableSpec, TableView};
use crate::app::ui::{self as app_ui, DialogButton, Palette};
use egui::{RichText, Ui};
use std::path::PathBuf;

/// What the dialog's job is doing, for its buttons and progress line.
pub struct JobView {
    pub waiting: bool,
    pub fraction: f32,
    pub label: String,
}

/// What was asked for in the dialog this frame.
pub enum PanelClick {
    Run,
    Stop,
    Hide,
    Row(RowClick),
}

/// What the app has to do after drawing the dialog.
pub enum PanelOutcome {
    Open,
    Hide,
    OpenRow(PathBuf),
    /// Start this plugin action on the row's folder, and come back after.
    RowAction {
        action: String,
        path: PathBuf,
    },
}

pub struct ResultsPanel {
    pub plugin: String,
    pub action: String,
    pub title: String,
    /// Where the next run acts: the folder, and the values once submitted.
    pub context: CallContext,
    pub state: FormState,
    /// Waiting for the plugin's `validate`.
    pub request: Option<u64>,
    /// The latest run. Updates from any other job are ignored.
    pub job: Option<u64>,
    /// The folder of the shown results.
    pub results_dir: Option<PathBuf>,
    pub table: Option<TableView>,
    /// Keeps the user's sort from one run to the next.
    pub last_sort: Option<SortOrder>,
    /// Keeps the user's size filter from one run to the next.
    pub last_filter: SizeFilter,
}

impl ResultsPanel {
    pub fn key(&self) -> String {
        format!("{}.{}", self.plugin, self.action)
    }

    /// The table a job of this panel sent: the first of a run, or an update.
    pub fn show(&mut self, spec: TableSpec) {
        if let Some(table) = self.table.as_mut() {
            table.update(spec);
        } else {
            let mut table = TableView::new(spec);
            if let Some(order) = self.last_sort {
                table.apply_sort(order);
            }
            table.set_filter(self.last_filter);
            self.table = Some(table);
        }
    }

    /// A new run starts: what it replaces is gone at once, so that old rows
    /// can't be mistaken for its results.
    pub fn started(&mut self, job: Option<u64>) {
        if let Some(table) = self.table.take() {
            self.last_sort = table.sorted_by();
            self.last_filter = table.filter();
        }
        self.results_dir.clone_from(&self.context.dir);
        self.job = job;
        self.state.message = None;
    }

    fn folder_name(&self) -> Option<String> {
        let dir = self.context.dir.as_ref()?;
        dir.file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .or_else(|| Some(dir.display().to_string()))
    }

    /// The buttons along the bottom: while a job runs it can be restarted,
    /// stopped or left to run; otherwise the dialog can run it or close.
    fn buttons_ui(&self, ui: &mut Ui, job: Option<&JobView>) -> Option<PanelClick> {
        let busy = self.state.busy;
        if job.is_some() {
            let buttons = [
                DialogButton::plain("Restart").enabled(!busy),
                DialogButton::plain("Stop"),
                DialogButton::plain("Run in background"),
            ];
            match app_ui::dialog_buttons(ui, false, &buttons) {
                Some(0) => Some(PanelClick::Run),
                Some(1) => Some(PanelClick::Stop),
                Some(_) => Some(PanelClick::Hide),
                None => None,
            }
        } else {
            let buttons = [
                DialogButton::primary("Run").enabled(!busy),
                DialogButton::plain("Close"),
            ];
            match app_ui::dialog_buttons(ui, false, &buttons) {
                Some(0) => Some(PanelClick::Run),
                Some(_) => Some(PanelClick::Hide),
                None => None,
            }
        }
    }

    /// The options on the left, and the size filter on the right, directly
    /// above the results they shape. Stacked when they don't fit side by side.
    fn toolbar_ui(&mut self, ui: &mut Ui) {
        const SIDE_BY_SIDE: f32 = 660.0;
        let stacked = ui.available_width() < SIDE_BY_SIDE;
        let (state, table) = (&mut self.state, &mut self.table);
        if stacked {
            ui.horizontal(|ui| state.toolbar_ui(ui));
            if let Some(table) = table.as_mut() {
                ui.horizontal(|ui| table.filter_ui(ui));
            }
        } else {
            ui.horizontal(|ui| {
                state.toolbar_ui(ui);
                if let Some(table) = table.as_mut() {
                    table.filter_ui(ui);
                }
            });
        }
    }

    /// While a job runs: how far along it is, and what it is doing.
    fn progress_ui(&self, ui: &mut Ui, job: &JobView) {
        let progress = self.table.as_ref().and_then(TableView::progress);
        let mut text = match (job.waiting, progress) {
            (true, _) => "Waiting for another job…".to_owned(),
            (false, Some(progress)) => progress.to_owned(),
            (false, None) => "Starting…".to_owned(),
        };
        if !job.waiting && !job.label.is_empty() {
            text = format!("{text} · {}", job.label);
        }
        ui.horizontal(|ui| {
            ui.add(
                egui::ProgressBar::new(job.fraction)
                    .desired_width(160.0)
                    .desired_height(6.0),
            );
            let secondary = Palette::of(ui).secondary;
            ui.add(egui::Label::new(RichText::new(text).color(secondary)).truncate());
        });
    }

    /// Where the results came from, when Run would now act somewhere else.
    fn other_folder_note(&self) -> Option<String> {
        let (results, next) = (self.results_dir.as_ref()?, self.context.dir.as_ref()?);
        (self.table.is_some() && results != next).then(|| {
            format!(
                "Showing {}. Run scans {}.",
                results.display(),
                next.display()
            )
        })
    }

    fn contents_ui(&mut self, ui: &mut Ui, job: Option<&JobView>) -> Option<PanelClick> {
        let mut click = None;
        let title = self.folder_name().map_or_else(
            || self.title.clone(),
            |folder| format!("{}: {folder}", self.title),
        );
        ui.heading(title);
        ui.add_space(12.0);
        self.toolbar_ui(ui);
        self.state.message_ui(ui);
        ui.add_space(12.0);
        if let Some(job) = job {
            self.progress_ui(ui, job);
            ui.add_space(4.0);
        }
        if let Some(note) = self.other_folder_note() {
            ui.label(RichText::new(note).color(Palette::of(ui).secondary));
            ui.add_space(4.0);
        }
        if let Some(table) = self.table.as_mut() {
            table.set_running(job.is_some());
            table.summary_ui(ui);
            ui.add_space(8.0);
        }
        // Keeps its height from before there are results, so the buttons
        // don't move when they arrive.
        let size = egui::vec2(
            ui.available_width(),
            (ui.ctx().content_rect().height() - 330.0).clamp(140.0, 620.0),
        );
        ui.allocate_ui(size, |ui| {
            ui.set_min_size(size);
            if let Some(table) = self.table.as_mut() {
                if let Some(row) = table.body_ui(ui) {
                    click = Some(PanelClick::Row(row));
                }
            } else {
                ui.centered_and_justified(|ui| {
                    ui.weak(if job.is_some() {
                        "Results appear here as they are found."
                    } else {
                        "Press Run to see what takes the most space."
                    });
                });
            }
        });
        if let Some(asked) = self.buttons_ui(ui, job) {
            click = Some(asked);
        }
        click
    }

    /// Draws the dialog. Hiding counts as a click: Esc and clicking outside
    /// mean the same as its button, and none of them stop the job.
    pub fn ui(&mut self, ctx: &egui::Context, job: Option<&JobView>) -> Option<PanelClick> {
        let mut click = None;
        // Esc or a click outside then closes that menu, not the dialog too.
        let popup_open = egui::Popup::is_any_open(ctx);
        let modal = egui::Modal::new(egui::Id::new("plugin_results"))
            .frame(app_ui::dialog_frame(ctx))
            .show(ctx, |ui| {
                ui.set_width((ctx.content_rect().width() - 64.0).clamp(480.0, 980.0));
                click = self.contents_ui(ui, job);
            });
        if click.is_none() && !popup_open && modal.should_close() {
            click = Some(PanelClick::Hide);
        }
        click
    }
}
