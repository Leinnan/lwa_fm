//! Drawing what plugins contribute: the directory bar, inspector sections,
//! the jobs indicator and the Plugins settings page.

use super::host::{BarView, InspectorView, PluginHost, SettingsMap, Status};
use super::{BarSegment, InspectorRow, Source};
use crate::app::ui::{self as app_ui, Palette};
use egui::{Color32, RichText, Ui};
use lucide_icons::Icon;

/// A lucide icon by its name (`git-branch`), or none for unknown names.
pub fn icon(name: Option<&str>) -> Option<Icon> {
    name.and_then(|name| Icon::try_from(name).ok())
}

fn tone_color(ui: &Ui, tone: Option<&str>) -> Option<Color32> {
    let visuals = ui.visuals();
    match tone {
        Some("muted") => Some(visuals.weak_text_color()),
        Some("good") => Some(Color32::from_rgb(46, 160, 67)),
        Some("warn") => Some(visuals.warn_fg_color),
        Some("bad") => Some(visuals.error_fg_color),
        _ => None,
    }
}

/// A segment; returns the action its button asks for.
fn segment_ui(ui: &mut Ui, plugin: &str, segment: &BarSegment) -> Option<String> {
    let color = tone_color(ui, segment.tone.as_deref());
    let icon = icon(segment.icon.as_deref());
    let mut clicked = None;
    let response = if let Some(label) = &segment.button {
        let mut text = RichText::new(label);
        if let Some(color) = color {
            text = text.color(color);
        }
        let button = match icon {
            Some(icon) => egui::Button::new((app_ui::glyph(icon).size(13.0), text)),
            None => egui::Button::new(text),
        };
        let response = ui.add(button.small());
        if response.clicked()
            && let Some(action) = &segment.action
        {
            clicked = Some(if action.contains('.') && action.starts_with(plugin) {
                action.clone()
            } else {
                format!("{plugin}.{action}")
            });
        }
        response
    } else if let Some(badge) = &segment.badge {
        let palette = Palette::of(ui);
        egui::Frame::new()
            .fill(palette.card)
            .corner_radius(8)
            .inner_margin(egui::Margin::symmetric(6, 1))
            .show(ui, |ui| {
                let mut text = RichText::new(badge).small();
                if let Some(color) = color {
                    text = text.color(color);
                }
                ui.label(text);
            })
            .response
    } else {
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 4.0;
            if let Some(icon) = icon {
                let mut glyph = app_ui::glyph(icon).size(13.0);
                if let Some(color) = color {
                    glyph = glyph.color(color);
                }
                ui.label(glyph);
            }
            if segment.spinner {
                ui.add(egui::Spinner::new().size(12.0));
            }
            if let Some(text) = &segment.text {
                let mut text = RichText::new(text);
                if let Some(color) = color {
                    text = text.color(color);
                }
                ui.label(text);
            }
        })
        .response
    };
    if let Some(tooltip) = &segment.tooltip {
        response.on_hover_text(tooltip);
    }
    clicked
}

/// Draws the plugins' bar for the folder; returns a clicked action's id.
pub fn bar_ui(ui: &mut Ui, views: &[BarView<'_>]) -> Option<String> {
    let mut clicked = None;
    ui.horizontal_wrapped(|ui| {
        for (index, view) in views.iter().enumerate() {
            if index > 0 {
                ui.separator();
            }
            ui.label(
                RichText::new(view.name)
                    .small()
                    .color(Palette::of(ui).secondary),
            );
            if let Some(error) = &view.state.error {
                ui.label(
                    app_ui::glyph(Icon::TriangleAlert)
                        .size(13.0)
                        .color(ui.visuals().warn_fg_color),
                )
                .on_hover_text(error);
            }
            for segment in &view.state.segments {
                if let Some(action) = segment_ui(ui, view.plugin, segment) {
                    clicked = Some(action);
                }
            }
            if view.state.loading {
                ui.add(egui::Spinner::new().size(12.0));
            }
        }
    });
    clicked
}

/// The plugins' sections in the inspector.
pub fn inspector_ui(ui: &mut Ui, views: &[InspectorView<'_>]) {
    for view in views {
        if view.state.rows.is_empty() && view.state.error.is_none() && !view.state.loading {
            continue;
        }
        app_ui::section(ui, view.title);
        app_ui::card(ui).show(ui, |ui| {
            ui.set_width(ui.available_width());
            if let Some(error) = &view.state.error {
                ui.colored_label(ui.visuals().error_fg_color, error);
            }
            for row in &view.state.rows {
                let (label, value) = match row {
                    InspectorRow::Pair(label, value) | InspectorRow::Named { label, value } => {
                        (label, value)
                    }
                };
                ui.label(
                    RichText::new(label)
                        .small()
                        .color(Palette::of(ui).secondary),
                );
                ui.add(egui::Label::new(value).wrap());
                ui.add_space(4.0);
            }
            if view.state.loading && view.state.rows.is_empty() {
                ui.spinner();
            }
        });
    }
}

/// Running jobs in the status bar, and a list of finished ones.
pub fn jobs_status(ui: &mut Ui, host: &mut PluginHost) {
    if !host.finished.is_empty() {
        ui.separator();
        let response = app_ui::icon_button(ui, Icon::Puzzle, "Plugin activity", false);
        egui::Popup::menu(&response).show(|ui| {
            ui.set_min_width(320.0);
            ui.strong("Plugin activity");
            for job in host.finished.iter().rev() {
                ui.horizontal(|ui| {
                    let (mark, color) = if job.ok {
                        ("✓", Palette::of(ui).accent)
                    } else {
                        ("✗", ui.visuals().error_fg_color)
                    };
                    ui.colored_label(color, mark);
                    let label = ui.label(&job.summary);
                    if !job.failures.is_empty() {
                        label.on_hover_text(job.failures.join("\n"));
                    }
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        ui.weak(format!("{}s ago", job.when.elapsed().as_secs()));
                    });
                });
            }
        });
    }
    let mut cancel = None;
    for job in &host.jobs {
        ui.separator();
        if job.waiting {
            ui.weak(format!("{} (waiting)", job.title));
        } else {
            ui.add(
                egui::ProgressBar::new(job.fraction)
                    .desired_width(140.0)
                    .show_percentage(),
            )
            .on_hover_text(if job.label.is_empty() {
                job.title.clone()
            } else {
                format!("{}\n{}", job.title, job.label)
            });
            ui.weak(&job.title);
        }
        if app_ui::icon_button(ui, Icon::X, "Cancel", false).clicked() {
            cancel = Some(job.id);
        }
    }
    if let Some(id) = cancel {
        host.cancel_job(id);
    }
    if host.has_jobs() {
        ui.ctx()
            .request_repaint_after(std::time::Duration::from_millis(250));
    }
}

fn status_text(status: &Status, enabled: bool) -> (&'static str, Option<String>) {
    match status {
        // Switched on; the worker starts on the next frame.
        Status::Off if enabled => ("Starting…", None),
        Status::Off => ("Off", None),
        Status::NeedsApproval => ("Needs approval", None),
        Status::Loading => ("Starting…", None),
        Status::Running => ("Running", None),
        Status::Failed(error) => ("Failed", Some(error.clone())),
    }
}

/// The Plugins page of the settings dialog.
#[allow(clippy::too_many_lines)]
pub fn settings_page(ui: &mut Ui, host: &mut PluginHost, settings: &mut SettingsMap) {
    let mut changed = false;
    app_ui::settings_group(ui, "Plugins folder", |group| {
        let dir = host.dir.clone();
        group.row(
            "Your plugins",
            Some(
                dir.as_ref()
                    .map_or_else(
                        || "No folder available".to_owned(),
                        |dir| dir.display().to_string(),
                    )
                    .as_str(),
            ),
            |ui| {
                if ui.button("Reload all").clicked() {
                    host.load_all();
                }
                if let Some(dir) = &dir
                    && ui.button("Open folder").clicked()
                {
                    let _ = std::fs::create_dir_all(dir);
                    if open::that_detached(dir).is_err() {
                        crate::toast!(Error, "Could not open {}", dir.display());
                    }
                }
            },
        );
        group.note(
            "Each plugin is a folder with plugin.toml and main.lua. Changes load as you save them.",
        );
    });
    let mut retry = None;
    let mut approve: Option<(String, u64)> = None;
    for plugin in &host.plugins {
        let id = plugin.id().to_owned();
        let manifest = &plugin.code.manifest;
        let enabled = plugin.enabled(settings);
        let approved = plugin.approved(settings);
        let source = match &plugin.code.source {
            Source::Bundled => "Included with DirFleet".to_owned(),
            Source::User(folder) => folder.display().to_string(),
        };
        app_ui::settings_group(ui, &manifest.name, |group| {
            let description = format!(
                "{}{}Version {} by {} · {source}",
                manifest.description,
                if manifest.description.is_empty() {
                    ""
                } else {
                    "\n"
                },
                manifest.version,
                manifest.author,
            );
            group.row("Enabled", Some(&description), |ui| {
                let mut on = enabled;
                // Enabling a plugin that asks for permissions waits for
                // "Allow" on its Permissions row.
                if app_ui::toggle_switch(ui, &mut on, &manifest.name).changed() {
                    settings.entry(id.clone()).or_default().enabled = Some(on);
                    changed = true;
                }
                let (text, detail) = status_text(&plugin.status, on);
                let label = ui.label(RichText::new(text).small().color(Palette::of(ui).secondary));
                if let Some(detail) = detail {
                    label.on_hover_text(detail);
                }
            });
            let permissions = plugin.permissions();
            if !permissions.is_empty() {
                let list: Vec<String> = permissions
                    .iter()
                    .map(super::permissions::Permission::describe)
                    .collect();
                let waiting = enabled && !approved;
                group.row("Permissions", Some(&list.join("\n")), |ui| {
                    if waiting {
                        if ui.button("Allow").clicked() {
                            approve = Some((id.clone(), permissions.fingerprint()));
                        }
                    } else if approved && !plugin.is_bundled() {
                        ui.label(RichText::new("Allowed").small());
                    }
                });
                if permissions.contains(&super::permissions::Permission::Ffmpeg) {
                    let (ffmpeg, _) = crate::app::assets::ffmpeg_tool_paths();
                    let found = ffmpeg.is_file();
                    group.row(
                        "FFmpeg",
                        Some(if found {
                            "Found"
                        } else {
                            "Not installed yet: it downloads the first time it's needed"
                        }),
                        |ui| {
                            if found {
                                ui.label(app_ui::glyph(Icon::Info).size(13.0))
                                    .on_hover_text(ffmpeg.display().to_string());
                            }
                        },
                    );
                }
            }
            for action in manifest
                .actions
                .iter()
                .filter(|action| action.palette || action.shortcut.is_some())
            {
                let full_id = format!("{id}.{}", action.id);
                let entry = settings.entry(id.clone()).or_default();
                let mut text = entry
                    .shortcuts
                    .get(&action.id)
                    .cloned()
                    .or_else(|| action.shortcut.clone())
                    .unwrap_or_default();
                let note = host.shortcut_notes.get(&full_id).cloned();
                group.row(
                    &action.title,
                    Some(note.as_deref().unwrap_or("Shortcut")),
                    |ui| {
                        let response = ui.add(
                            app_ui::text_field(ui, &mut text)
                                .hint_text("None")
                                .desired_width(120.0),
                        );
                        if response.changed() {
                            if action.shortcut.as_deref() == Some(text.as_str()) {
                                entry.shortcuts.remove(&action.id);
                            } else {
                                entry.shortcuts.insert(action.id.clone(), text.clone());
                            }
                        }
                        if response.lost_focus() {
                            changed = true;
                        }
                    },
                );
            }
            if !plugin.errors.is_empty() {
                let errors: Vec<&str> = plugin
                    .errors
                    .iter()
                    .rev()
                    .take(5)
                    .map(String::as_str)
                    .collect();
                group.row("Recent errors", Some(&errors.join("\n")), |ui| {
                    if ui.button("Copy").clicked() {
                        ui.ctx().copy_text(errors.join("\n"));
                    }
                    if matches!(plugin.status, Status::Failed(_)) && ui.button("Retry").clicked() {
                        retry = Some(id.clone());
                    }
                });
            }
        });
    }
    if !host.broken.is_empty() {
        app_ui::settings_group(ui, "Couldn't load", |group| {
            for broken in &host.broken {
                let name = broken
                    .folder
                    .file_name()
                    .map(|name| name.to_string_lossy().into_owned())
                    .unwrap_or_default();
                group.row(&name, Some(&broken.diagnostic.to_string()), |_| {});
            }
        });
    }
    if let Some((id, fingerprint)) = approve {
        let entry = settings.entry(id).or_default();
        entry.approved = Some(fingerprint);
        entry.enabled = Some(true);
        changed = true;
    }
    if let Some(id) = retry {
        host.retry(&id);
    }
    if changed {
        host.shortcuts_changed();
    }
}
