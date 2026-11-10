//! Plugin jobs (`mode = "job"`), as egui's: each on a thread of its own with
//! progress and Cancel, at most [`MAX_JOBS`] at once and at most the plugin's
//! own `max_jobs`; the rest wait their turn. A finished job says how it went.
use crux_core::Command;
use crux_core::render::render;
use facet::Facet;
use serde::{Deserialize, Serialize};

use super::plugins::{PluginControl, control};
use super::{DirFleet, Effect, Event, Model, ToastKind};
use crate::plugins::manifest::RunMode;
use crate::plugins::{CallContext, JobEvent, Request};

/// Jobs running at once, of every plugin.
const MAX_JOBS: usize = 2;

pub(super) struct Job {
    pub(super) id: u64,
    pub(super) plugin: String,
    pub(super) title: String,
    /// 0 to 1.
    pub(super) fraction: f32,
    pub(super) label: String,
    pub(super) per_item: bool,
    /// Queued behind other jobs; it has its action and context until it starts.
    pub(super) waiting: Option<(String, CallContext)>,
    /// A newer run replaced it, or it measures a row again: nobody needs
    /// to hear it ended.
    pub(super) quiet: bool,
}

/// A job, for the progress list.
#[derive(Facet, Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct PluginJobView {
    pub id: u64,
    pub title: String,
    /// Done so far, in thousandths.
    pub progress: u16,
    /// What it is doing now.
    pub label: String,
    /// Waiting for another job to end.
    pub waiting: bool,
    /// The results dialog it fills, to bring back; "" for none.
    pub results: String,
}

impl Job {
    pub(super) fn view(&self) -> PluginJobView {
        PluginJobView {
            id: self.id,
            title: self.title.clone(),
            #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
            progress: (self.fraction.clamp(0.0, 1.0) * 1000.0).round() as u16,
            label: self.label.clone(),
            waiting: self.waiting.is_some(),
            results: String::new(),
        }
    }

    /// How it went, as egui says it.
    fn summary(&self, done: usize, skipped: usize, failures: &[String], cancelled: bool) -> String {
        if cancelled {
            return format!("{}: cancelled", self.title);
        }
        if !self.per_item {
            return failures.first().map_or_else(
                || format!("{}: done", self.title),
                |error| format!("{} failed: {error}", self.title),
            );
        }
        let mut parts = vec![format!("{done} done")];
        if skipped > 0 {
            parts.push(format!("{skipped} skipped"));
        }
        if !failures.is_empty() {
            parts.push(format!("{} failed", failures.len()));
        }
        format!("{}: {}", self.title, parts.join(", "))
    }
}

impl DirFleet {
    /// Runs an action, its form answered: on the plugin's worker, or as a
    /// job, whose number comes back.
    pub(super) fn dispatch_plugin_action(
        model: &mut Model,
        plugin: &str,
        action: &str,
        context: CallContext,
    ) -> (Command<Effect, Event>, Option<u64>) {
        let Some(def) = model.plugins.action(plugin, action) else {
            return (Command::done(), None);
        };
        if def.mode == RunMode::Quick {
            let run = control(PluginControl::Send {
                id: plugin.to_owned(),
                request: Request::Run {
                    action: action.to_owned(),
                    context,
                },
            });
            return (run, None);
        }
        let mut title = def.title.trim_end_matches('…').to_owned();
        if let [target] = context.targets.as_slice() {
            title = format!("{title}: {}", target.name());
        } else if context.targets.len() > 1 {
            title = format!("{title} ({} items)", context.targets.len());
        }
        let per_item = def.per_item;
        let plugins = &mut model.plugins;
        plugins.next_job += 1;
        plugins.jobs.push(Job {
            id: plugins.next_job,
            plugin: plugin.to_owned(),
            title,
            fraction: 0.0,
            label: String::new(),
            per_item,
            waiting: Some((action.to_owned(), context)),
            quiet: false,
        });
        let id = plugins.next_job;
        (render().and(Self::start_plugin_jobs(model)), Some(id))
    }

    /// Starts waiting jobs while there is room, oldest first.
    fn start_plugin_jobs(model: &mut Model) -> Command<Effect, Event> {
        let plugins = &mut model.plugins;
        let mut commands = Vec::new();
        for index in 0..plugins.jobs.len() {
            let running = plugins
                .jobs
                .iter()
                .filter(|job| job.waiting.is_none())
                .count();
            if running >= MAX_JOBS {
                break;
            }
            let job = &plugins.jobs[index];
            if job.waiting.is_none() {
                continue;
            }
            let limit = plugins.max_jobs(&job.plugin);
            let mine = plugins
                .jobs
                .iter()
                .filter(|other| other.waiting.is_none() && other.plugin == job.plugin)
                .count();
            if mine >= limit {
                continue;
            }
            let job = &mut plugins.jobs[index];
            let Some((action, context)) = job.waiting.take() else {
                continue;
            };
            commands.push(control(PluginControl::StartJob {
                job: job.id,
                id: job.plugin.clone(),
                action,
                context,
                per_item: job.per_item,
            }));
        }
        Command::all(commands)
    }

    /// Marks a job as one nobody needs to hear the end of.
    pub(super) fn quiet_plugin_job(model: &mut Model, id: u64) {
        if let Some(job) = model.plugins.jobs.iter_mut().find(|job| job.id == id) {
            job.quiet = true;
        }
    }

    pub(super) fn cancel_plugin_job(model: &mut Model, id: u64) -> Command<Effect, Event> {
        let plugins = &mut model.plugins;
        let Some(index) = plugins.jobs.iter().position(|job| job.id == id) else {
            return Command::done();
        };
        // One still waiting just goes.
        if plugins.jobs[index].waiting.is_some() {
            plugins.jobs.remove(index);
            return render();
        }
        control(PluginControl::CancelJob { job: id })
    }

    pub(super) fn plugin_job_event(
        model: &mut Model,
        id: u64,
        event: JobEvent,
    ) -> Command<Effect, Event> {
        let plugins = &mut model.plugins;
        let Some(index) = plugins.jobs.iter().position(|job| job.id == id) else {
            return Command::done();
        };
        match event {
            JobEvent::Progress { fraction, label } => {
                let job = &mut plugins.jobs[index];
                job.fraction = fraction;
                job.label = label;
                render()
            }
            JobEvent::Finished {
                done,
                skipped,
                failures,
                cancelled,
            } => {
                let job = plugins.jobs.remove(index);
                let results = Self::results_job_finished(model, id, &failures);
                if job.quiet {
                    return results.and(Self::start_plugin_jobs(model));
                }
                let summary = job.summary(done, skipped, &failures, cancelled);
                model.plugins.views.invalidate(&job.plugin, true, true);
                let finished = Self::emit_plugin_event(
                    model,
                    "job_finished",
                    &CallContext::default(),
                    &serde_json::json!({
                        "title": job.title,
                        "done": done,
                        "skipped": skipped,
                        "cancelled": cancelled,
                    }),
                );
                let kind = if cancelled {
                    ToastKind::Info
                } else if failures.is_empty() {
                    ToastKind::Success
                } else {
                    ToastKind::Error
                };
                results
                    .and(Self::toast(model, kind, summary))
                    .and(finished)
                    .and(Self::start_plugin_jobs(model))
            }
        }
    }
}
