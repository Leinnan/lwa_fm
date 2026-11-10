//! The results dialog of an action with `results = true`, as egui's: its
//! options, Run, Stop and Run in background, and the table its job fills in
//! (`egui_mev_data_core`'s `Table`, rows standing for folders). Hiding it
//! leaves the job running; the job's progress line and the action bring it
//! back. A row's menu runs the plugin's actions on its folder; the row says
//! what is happening and is measured again once they end.
use std::collections::BTreeMap;
use std::path::PathBuf;

use crux_core::Command;
use crux_core::render::render;
use facet::Facet;
use serde::{Deserialize, Serialize};

use super::plugin_forms::{FormFieldView, OpenForm};
use super::plugins::{PluginControl, control};
use super::{DirFleet, Effect, Event, Model};
use crate::plugins::{CallContext, Form, Request, RowUpdate, Table, TableSpec};
use egui_mev_data_core::table::{
    ColumnKind, RowStatus, SizeFilter, SizeUnit, SortOrder, TableLabels, cell_text,
};

/// A result row an action was started from.
#[derive(Clone, Debug)]
pub(super) struct RowOrigin {
    /// The dialog's key.
    pub(super) panel: String,
    pub(super) path: PathBuf,
    /// What the row says while the action runs.
    pub(super) busy: String,
}

pub(super) struct Panel {
    pub(super) plugin: String,
    pub(super) action: String,
    title: String,
    /// Where the next run acts: the folder, and the values once submitted.
    context: CallContext,
    /// The options, with their values, errors and the last request.
    pub(super) form: OpenForm,
    /// The latest run; updates from other jobs are ignored.
    pub(super) job: Option<u64>,
    /// The folder of the shown results.
    results_dir: Option<PathBuf>,
    table: Option<Table>,
    /// The user's sort and size filter, kept from one run to the next.
    last_sort: Option<SortOrder>,
    last_filter: SizeFilter,
}

#[derive(Facet, Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct ResultsColumnView {
    pub title: String,
    /// Sizes and dates: right-aligned.
    pub numeric: bool,
    pub sorted: bool,
    pub descending: bool,
}

#[derive(Facet, Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct ResultsMenuItemView {
    pub action: String,
    pub label: String,
    pub danger: bool,
}

#[derive(Facet, Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct ResultsRowView {
    /// The folder it stands for; "" for a row that does nothing.
    pub path: String,
    pub cells: Vec<String>,
    pub tooltip: String,
    /// "Waiting…", what its action is doing, "Measuring…", "Failed" or "".
    pub status: String,
    /// Why it failed, or "".
    pub failure: String,
    /// Something runs on it: its menu waits.
    pub busy: bool,
    pub menu: Vec<ResultsMenuItemView>,
    /// Its menu's actions can run now.
    pub actions_enabled: bool,
}

#[derive(Facet, Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
#[allow(clippy::struct_excessive_bools)]
pub struct PluginResultsView {
    /// `<plugin id>.<action id>`.
    pub key: String,
    /// The tab whose window shows it.
    pub tab: u32,
    /// "Folder Usage: Documents".
    pub title: String,
    /// The options, in a row above the results.
    pub fields: Vec<FormFieldView>,
    pub message: String,
    /// The plugin is checking the options.
    pub busy: bool,
    /// Its job runs (or waits): Restart, Stop and Run in background.
    pub running: bool,
    /// Done so far, in thousandths, and what it is doing.
    pub progress: u16,
    pub progress_text: String,
    /// Where the results came from, when Run would now act elsewhere.
    pub note: String,
    pub subtitle: String,
    /// "Updated just now", "Stopped early" or "".
    pub summary: String,
    pub has_table: bool,
    pub columns: Vec<ResultsColumnView>,
    pub rows: Vec<ResultsRowView>,
    /// Instead of rows when there are none, or before there is a table.
    pub empty_text: String,
    /// The size filter, when there are sizes.
    pub has_sizes: bool,
    pub filter_amount: String,
    pub filter_gigabytes: bool,
    /// "Showing 3 of 10" while the filter hides rows.
    pub filter_text: String,
    /// The built-in row menu entries, as the dialog names them; refresh is
    /// "" without a refresh action.
    pub open_label: String,
    pub refresh_label: String,
    pub copy_label: String,
}

/// The labels of the folder rows' built-in menu entries, as egui's.
fn labels() -> TableLabels {
    TableLabels {
        open: "Open in new tab".into(),
        refresh: "Measure again".into(),
        copy: "Copy path".into(),
        job_running: "Stop the scan first".into(),
        row_busy: "Wait for this folder to finish".into(),
    }
}

impl Panel {
    pub(super) fn key(&self) -> String {
        format!("{}.{}", self.plugin, self.action)
    }

    /// The table a job of this dialog sent: the first of a run, or an update.
    fn show(&mut self, spec: TableSpec, running: bool) {
        if let Some(table) = self.table.as_mut() {
            table.update(spec);
        } else {
            let mut table = Table::new(spec);
            if let Some(order) = self.last_sort {
                table.apply_sort(order);
            }
            table.set_filter(self.last_filter);
            self.table = Some(table);
        }
        if let Some(table) = self.table.as_mut() {
            table.set_running(running);
        }
    }

    /// A new run starts: what it replaces goes at once, so old rows can't
    /// be taken for its results.
    fn started(&mut self, job: Option<u64>) {
        if let Some(table) = self.table.take() {
            self.last_sort = table.sorted_by();
            self.last_filter = table.filter();
        }
        self.results_dir.clone_from(&self.context.dir);
        self.job = job;
        self.form.message = None;
    }

    fn title(&self) -> String {
        let folder = self.context.dir.as_ref().map(|dir| {
            dir.file_name().map_or_else(
                || dir.to_string_lossy().into_owned(),
                |name| name.to_string_lossy().into_owned(),
            )
        });
        folder.map_or_else(
            || self.title.clone(),
            |folder| format!("{}: {folder}", self.title),
        )
    }

    fn row_view(
        table: &Table,
        index: usize,
        row_jobs: &[(u64, RowOrigin, bool)],
    ) -> ResultsRowView {
        let spec = table.spec();
        let row = &spec.rows[index];
        let path = row.payload.clone().unwrap_or_default();
        // A job the row's menu started speaks for it while it runs.
        let live = row_jobs
            .iter()
            .find(|(_, origin, _)| origin.path == path)
            .map(|(_, origin, waiting)| {
                if *waiting {
                    RowStatus::Waiting
                } else {
                    RowStatus::Working(origin.busy.clone())
                }
            });
        let status = live.or_else(|| {
            row.payload
                .as_ref()
                .and_then(|path| table.status(path).cloned())
        });
        ResultsRowView {
            path: path.to_string_lossy().into_owned(),
            cells: spec
                .columns
                .iter()
                .map(|column| cell_text(column.kind, row.cells.get(&column.id)))
                .collect(),
            tooltip: row.tooltip.clone().unwrap_or_default(),
            status: status
                .as_ref()
                .map(|status| status.text().to_owned())
                .unwrap_or_default(),
            failure: match &status {
                Some(RowStatus::Failed(reason)) => reason.clone(),
                _ => String::new(),
            },
            busy: status.as_ref().is_some_and(RowStatus::is_busy),
            menu: row
                .menu
                .iter()
                .map(|item| ResultsMenuItemView {
                    action: item.action.clone(),
                    label: item.label.clone(),
                    danger: item.danger,
                })
                .collect(),
            actions_enabled: !table.running() && !status.as_ref().is_some_and(RowStatus::is_busy),
        }
    }

    fn view(
        &self,
        tab: u32,
        job: Option<&super::plugin_jobs::Job>,
        row_jobs: &[(u64, RowOrigin, bool)],
    ) -> PluginResultsView {
        let mut fields = Vec::new();
        if let Some(form) = &self.form.form {
            self.form.fields_view(&form.fields, true, 0, &mut fields);
        }
        let table = self.table.as_ref();
        let progress_text = job.map(|job| {
            let progress = table.and_then(Table::progress);
            let text = match (job.waiting.is_some(), progress) {
                (true, _) => "Waiting for another job…".to_owned(),
                (false, Some(progress)) => progress.to_owned(),
                (false, None) => "Starting…".to_owned(),
            };
            if job.waiting.is_none() && !job.label.is_empty() {
                format!("{text} · {}", job.label)
            } else {
                text
            }
        });
        let note = match (&self.results_dir, &self.context.dir) {
            (Some(results), Some(next)) if table.is_some() && results != next => format!(
                "Showing {}. Run scans {}.",
                results.display(),
                next.display()
            ),
            _ => String::new(),
        };
        let labels = labels();
        PluginResultsView {
            key: self.key(),
            tab,
            title: self.title(),
            fields,
            message: self.form.message.clone().unwrap_or_default(),
            busy: self.form.busy,
            running: job.is_some(),
            #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
            progress: job.map_or(0, |job| {
                (job.fraction.clamp(0.0, 1.0) * 1000.0).round() as u16
            }),
            progress_text: progress_text.unwrap_or_default(),
            note,
            subtitle: table.and_then(Table::subtitle).unwrap_or_default(),
            summary: table.and_then(Table::summary).unwrap_or_default(),
            has_table: table.is_some(),
            columns: table.map_or_else(Vec::new, |table| {
                table
                    .spec()
                    .columns
                    .iter()
                    .enumerate()
                    .map(|(index, column)| ResultsColumnView {
                        title: column.label.clone(),
                        numeric: column.kind != ColumnKind::Text,
                        sorted: table.sorted_by().is_some_and(|(sorted, _)| sorted == index),
                        descending: table.sorted_by().is_some_and(|(_, descending)| descending),
                    })
                    .collect()
            }),
            rows: table.map_or_else(Vec::new, |table| {
                table
                    .visible()
                    .into_iter()
                    .map(|index| Self::row_view(table, index, row_jobs))
                    .collect()
            }),
            empty_text: match table {
                Some(table) => table.empty_text().unwrap_or_default().to_owned(),
                None if job.is_some() => "Results appear here as they are found.".to_owned(),
                None => "Press Run to see what takes the most space.".to_owned(),
            },
            has_sizes: table.is_some_and(Table::has_sizes),
            filter_amount: table.map_or_else(String::new, |table| {
                let amount = table.filter().amount;
                if amount == 0.0 {
                    String::new()
                } else {
                    amount.to_string()
                }
            }),
            filter_gigabytes: table.is_none_or(|table| table.filter().unit == SizeUnit::Gb),
            filter_text: table.and_then(Table::filter_text).unwrap_or_default(),
            open_label: labels.open,
            refresh_label: if table.is_some_and(|table| table.refresh_action().is_some()) {
                labels.refresh
            } else {
                String::new()
            },
            copy_label: labels.copy,
        }
    }
}

impl DirFleet {
    /// Shows the action's results dialog, making it first if need be. A run
    /// in progress keeps its folder; otherwise the next run acts on `context`.
    pub(super) fn open_results(
        model: &mut Model,
        plugin: &str,
        action: &str,
        context: CallContext,
    ) -> Command<Effect, Event> {
        let key = format!("{plugin}.{action}");
        let running: Vec<u64> = model.plugins.jobs.iter().map(|job| job.id).collect();
        if let Some(panel) = model
            .plugins
            .panels
            .iter_mut()
            .find(|panel| panel.key() == key)
        {
            if !panel.job.is_some_and(|job| running.contains(&job)) {
                panel.context = context;
            }
        } else {
            let Some(def) = model.plugins.action(plugin, action).cloned() else {
                return Command::done();
            };
            let form = Form {
                title: Some(def.title.clone()),
                remember: true,
                fields: def.fields,
                ..Default::default()
            };
            let mut state = OpenForm::new(
                plugin.to_owned(),
                action.to_owned(),
                def.title.clone(),
                context.clone(),
                0,
            );
            state.show(form, model.plugins.remembered(plugin, action));
            model.plugins.panels.push(Panel {
                plugin: plugin.to_owned(),
                action: action.to_owned(),
                title: def.title.trim_end_matches('…').to_owned(),
                context,
                form: state,
                job: None,
                results_dir: None,
                table: None,
                last_sort: None,
                last_filter: SizeFilter::default(),
            });
        }
        model.plugins.shown_panel = Some(key);
        render()
    }

    /// The shown dialog, unless a form opened from it is in front.
    pub(super) fn results_view(model: &Model) -> Option<PluginResultsView> {
        let plugins = &model.plugins;
        if plugins.form.is_some() {
            return None;
        }
        let key = plugins.shown_panel.as_deref()?;
        let panel = plugins.panels.iter().find(|panel| panel.key() == key)?;
        let job = panel
            .job
            .and_then(|id| plugins.jobs.iter().find(|job| job.id == id));
        let row_jobs: Vec<(u64, RowOrigin, bool)> = plugins
            .row_jobs
            .iter()
            .filter(|(_, origin)| origin.panel == key)
            .filter_map(|(id, origin)| {
                let job = plugins.jobs.iter().find(|job| job.id == *id)?;
                Some((*id, origin.clone(), job.waiting.is_some()))
            })
            .collect();
        Some(panel.view(panel.context.tab.unwrap_or_default(), job, &row_jobs))
    }

    fn shown_panel(model: &mut Model) -> Option<&mut Panel> {
        let key = model.plugins.shown_panel.clone()?;
        model
            .plugins
            .panels
            .iter_mut()
            .find(|panel| panel.key() == key)
    }

    /// The dialog's events.
    pub(super) fn results_event(model: &mut Model, event: Event) -> Command<Effect, Event> {
        match event {
            Event::RunPluginResults => Self::run_results(model),
            Event::StopPluginResults => {
                let Some(job) = Self::shown_panel(model).and_then(|panel| panel.job) else {
                    return Command::done();
                };
                Self::cancel_plugin_job(model, job)
            }
            Event::HidePluginResults => {
                model.plugins.shown_panel = None;
                render()
            }
            Event::ShowPluginResults { key } => {
                if model.plugins.panels.iter().any(|panel| panel.key() == key) {
                    model.plugins.shown_panel = Some(key);
                }
                render()
            }
            Event::SetPluginResultsField { id, text } => {
                if let Some(panel) = Self::shown_panel(model) {
                    panel.form.set(&id, text);
                }
                render()
            }
            Event::SortPluginResults { column } => {
                if let Some(table) = Self::shown_panel(model).and_then(|panel| panel.table.as_mut())
                    && (column as usize) < table.spec().columns.len()
                {
                    table.click_header(column as usize);
                }
                render()
            }
            Event::FilterPluginResults { amount, gigabytes } => {
                if let Some(table) = Self::shown_panel(model).and_then(|panel| panel.table.as_mut())
                {
                    table.set_filter(SizeFilter {
                        amount: amount
                            .trim()
                            .parse::<f64>()
                            .ok()
                            .filter(|n| n.is_finite() && *n >= 0.0)
                            .unwrap_or(0.0),
                        unit: if gigabytes {
                            SizeUnit::Gb
                        } else {
                            SizeUnit::Mb
                        },
                    });
                }
                render()
            }
            Event::OpenPluginResultsRow { path } => {
                model.plugins.shown_panel = None;
                Self::new_window(model, Some(path))
            }
            Event::RefreshPluginResultsRow { path } => {
                let Some(key) = model.plugins.shown_panel.clone() else {
                    return Command::done();
                };
                let row = RowOrigin {
                    panel: key,
                    path: PathBuf::from(path),
                    busy: String::new(),
                };
                Self::measure_row_again(model, &row)
            }
            Event::PluginResultsRowAction { path, action } => {
                Self::row_action(model, &path, &action)
            }
            _ => Command::done(),
        }
    }

    /// Run: the options' own rules, then the plugin's validate.
    fn run_results(model: &mut Model) -> Command<Effect, Event> {
        model.plugins.next_request += 1;
        let request = model.plugins.next_request;
        let Some(panel) = Self::shown_panel(model) else {
            return Command::done();
        };
        if panel.form.busy {
            return Command::done();
        }
        let Some(form) = panel.form.form.as_ref() else {
            return Command::done();
        };
        match form.validate(&panel.form.values) {
            Ok(values) => {
                panel.form.values = values;
                panel.form.errors.clear();
            }
            Err(errors) => {
                panel.form.errors = errors;
                return render();
            }
        }
        panel.form.busy = true;
        panel.form.request = request;
        let mut context = panel.context.clone();
        context.values = Some(panel.form.values.clone());
        render().and(control(PluginControl::Send {
            id: panel.plugin.clone(),
            request: Request::Validate {
                request,
                action: panel.action.clone(),
                context,
            },
        }))
    }

    /// The plugin checked the options: a new run replaces the last one.
    pub(super) fn results_validated(
        model: &mut Model,
        request: u64,
        result: Result<BTreeMap<String, String>, String>,
    ) -> Command<Effect, Event> {
        let Some(index) = model
            .plugins
            .panels
            .iter()
            .position(|panel| panel.form.request == request)
        else {
            return Command::done();
        };
        let panel = &mut model.plugins.panels[index];
        panel.form.busy = false;
        match result {
            Ok(errors) if errors.is_empty() => {
                let (plugin, action) = (panel.plugin.clone(), panel.action.clone());
                let values = panel.form.values.clone();
                panel.context.values = Some(values.clone());
                let context = panel.context.clone();
                let replaced = panel.job;
                model
                    .plugins
                    .settings
                    .entry(plugin.clone())
                    .or_default()
                    .remembered
                    .insert(action.clone(), values);
                let mut command = render();
                if let Some(id) = replaced {
                    Self::quiet_plugin_job(model, id);
                    command = command.and(Self::cancel_plugin_job(model, id));
                }
                let (run, job) = Self::dispatch_plugin_action(model, &plugin, &action, context);
                model.plugins.panels[index].started(job);
                command.and(run)
            }
            Ok(mut errors) => {
                panel.form.message = errors.remove("");
                panel.form.errors = errors;
                render()
            }
            Err(error) => {
                panel.form.message = Some(error);
                render()
            }
        }
    }

    /// A job of a dialog sent its table.
    pub(super) fn results_table(
        model: &mut Model,
        plugin: &str,
        job: Option<u64>,
        spec: TableSpec,
    ) -> Command<Effect, Event> {
        let Some(job) = job else {
            return Command::done();
        };
        let running = model.plugins.jobs.iter().any(|each| each.id == job);
        if let Some(panel) = model
            .plugins
            .panels
            .iter_mut()
            .find(|panel| panel.plugin == plugin && panel.job == Some(job))
        {
            panel.show(spec, running);
        }
        render()
    }

    /// A change to one row of a plugin's results.
    pub(super) fn results_row(
        model: &mut Model,
        plugin: &str,
        update: RowUpdate,
    ) -> Command<Effect, Event> {
        if let Some(table) = model
            .plugins
            .panels
            .iter_mut()
            .filter(|panel| panel.plugin == plugin)
            .filter_map(|panel| panel.table.as_mut())
            .find(|table| table.has_row(update.payload.as_path()))
        {
            table.update_row(update);
        }
        render()
    }

    fn set_row_status(model: &mut Model, row: &RowOrigin, status: Option<RowStatus>) {
        if let Some(table) = model
            .plugins
            .panels
            .iter_mut()
            .find(|panel| panel.key() == row.panel)
            .and_then(|panel| panel.table.as_mut())
        {
            table.set_status(row.path.as_path(), status);
        }
    }

    /// Runs the table's refresh action for one row, without announcing it.
    fn measure_row_again(model: &mut Model, row: &RowOrigin) -> Command<Effect, Event> {
        let Some(panel) = model
            .plugins
            .panels
            .iter()
            .find(|panel| panel.key() == row.panel)
        else {
            return Command::done();
        };
        let Some(table) = panel
            .table
            .as_ref()
            .filter(|table| table.has_row(row.path.as_path()))
        else {
            return Command::done();
        };
        let Some(action) = table.refresh_action().map(ToOwned::to_owned) else {
            Self::set_row_status(model, row, None);
            return render();
        };
        let plugin = panel.plugin.clone();
        let context = CallContext {
            dir: Some(row.path.clone()),
            tab: panel.context.tab,
            ..Default::default()
        };
        let (command, job) = Self::dispatch_plugin_action(model, &plugin, &action, context);
        if let Some(id) = job {
            Self::quiet_plugin_job(model, id);
            model.plugins.measure_jobs.push((id, row.clone()));
            Self::set_row_status(model, row, Some(RowStatus::Measuring));
        }
        render().and(command)
    }

    /// One of a row's own actions, on its folder.
    fn row_action(model: &mut Model, path: &str, action: &str) -> Command<Effect, Event> {
        let Some(panel) = Self::shown_panel(model) else {
            return Command::done();
        };
        let path = PathBuf::from(path);
        let Some(item) = panel.table.as_ref().and_then(|table| {
            table
                .spec()
                .rows
                .iter()
                .find(|row| row.payload.as_ref() == Some(&path))?
                .menu
                .iter()
                .find(|item| item.action == action)
                .cloned()
        }) else {
            return Command::done();
        };
        let row = RowOrigin {
            panel: panel.key(),
            path: path.clone(),
            busy: item.busy.clone().unwrap_or_else(|| "Working…".to_owned()),
        };
        let plugin = panel.plugin.clone();
        let context = CallContext {
            dir: Some(path),
            tab: panel.context.tab,
            ..Default::default()
        };
        Self::start_plugin_action(model, &plugin, action, context, Some((row, item.title)))
    }

    /// A job ended: the dialog it filled stops running and says what failed;
    /// a row it acted on is measured again, or says it failed.
    pub(super) fn results_job_finished(
        model: &mut Model,
        id: u64,
        failures: &[String],
    ) -> Command<Effect, Event> {
        let failed = (!failures.is_empty()).then(|| failures.join("\n"));
        for panel in &mut model.plugins.panels {
            if panel.job == Some(id) {
                if let Some(table) = panel.table.as_mut() {
                    table.set_running(false);
                }
                if let Some(failed) = &failed {
                    panel.form.message = Some(failed.clone());
                }
            }
        }
        if let Some(index) = model
            .plugins
            .measure_jobs
            .iter()
            .position(|(job, _)| *job == id)
        {
            let (_, row) = model.plugins.measure_jobs.remove(index);
            // Its update has arrived by now, and cleared the row, or it failed.
            Self::set_row_status(model, &row, failed.clone().map(RowStatus::Failed));
        }
        let Some(index) = model
            .plugins
            .row_jobs
            .iter()
            .position(|(job, _)| *job == id)
        else {
            return render();
        };
        let (_, row) = model.plugins.row_jobs.remove(index);
        match failed {
            Some(failed) => {
                Self::set_row_status(model, &row, Some(RowStatus::Failed(failed)));
                render()
            }
            None => Self::measure_row_again(model, &row),
        }
    }
}
