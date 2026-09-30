use super::{
    App,
    assets::{HoverPreview, PreviewIntent},
    commands::{ActionToPerform, TabAction},
    dock::Selected,
    ui::{self, Palette},
};
use crate::{
    data::{files::DirEntry, time::TimestampSeconds},
    helper::DataHolder,
};
use egui::{Context, Ui, Vec2};
use std::path::PathBuf;

#[derive(Clone, Debug)]
pub struct InspectorView {
    pub tab_id: u32,
    pub paths: Vec<PathBuf>,
    pub entry: Option<DirEntry>,
}
impl App {
    pub(crate) fn inspector_view(&mut self, ctx: &Context) -> Option<InspectorView> {
        let tab = self.tabs.get_current_tab()?;
        let selected = ctx
            .data_get_path::<Selected>(&tab.current_path)
            .unwrap_or_default();
        let entries: Vec<_> = selected
            .selected_fields
            .iter()
            .filter_map(|&index| tab.entry_at(index))
            .collect();
        Some(InspectorView {
            tab_id: tab.id,
            paths: entries.iter().map(DirEntry::get_path).collect(),
            entry: if entries.len() == 1 {
                entries.into_iter().next()
            } else {
                None
            },
        })
    }
    #[allow(clippy::too_many_lines)]
    pub(crate) fn inspector_ui(&mut self, ui: &mut Ui) {
        let view = self.inspector_view(ui.ctx());
        ui.heading("File details");
        ui.separator();
        egui::ScrollArea::vertical()
            .id_salt("inspector_scroll")
            .show(ui, |ui| {
                let Some(view) = view else {
                    ui.weak("Select a file to see its details.");
                    return;
                };
                if view.paths.is_empty() {
                    ui.add_space(24.0);
                    ui.weak("Select a file to see its preview and details.");
                    return;
                }
                if view.paths.len() > 1 {
                    ui::section(ui, "Selection");
                    ui::badge(ui, &format!("{} items selected", view.paths.len()));
                    ui.weak("Select one item to see its preview and metadata.");
                    return;
                }
                let Some(entry) = view.entry else {
                    return;
                };
                ui.add_space(8.0);
                let width = ui.available_width();
                ui::card(ui).show(ui, |ui| {
                    let target = Vec2::splat((width - 24.0).clamp(96.0, 300.0));
                    match self.assets.request_hover_preview_at_size(
                        ui.ctx(),
                        &entry,
                        PreviewIntent::Selected,
                        target,
                    ) {
                        HoverPreview::Ready(texture) => {
                            ui.add(
                                egui::Image::new(&texture)
                                    .maintain_aspect_ratio(true)
                                    .max_size(target),
                            );
                        }
                        HoverPreview::Pending => {
                            ui.spinner();
                            ui.weak("Loading preview…");
                        }
                        HoverPreview::Unavailable {
                            reason,
                            retry_after,
                        } => {
                            ui.weak("Preview unavailable");
                            ui.small(reason);
                            if let Some(delay) = retry_after {
                                ui.small(format!("Retrying in {} seconds", delay.as_secs().max(1)));
                            }
                        }
                        HoverPreview::Fallback => {
                            if let Some(texture) = self.assets.request_entry_texture(&entry) {
                                ui.add(
                                    egui::Image::new(&texture).fit_to_exact_size(Vec2::splat(64.0)),
                                );
                            }
                            ui.weak(if entry.is_file() {
                                "No preview available"
                            } else {
                                "Folder"
                            });
                        }
                    }
                });
                ui::section(ui, "Information");
                ui::card(ui).show(ui, |ui| {
                    ui.add(egui::Label::new(egui::RichText::new(&entry.file_name).strong()).wrap());
                    ui.add(egui::Label::new(entry.full_path_string()).wrap())
                        .on_hover_text(entry.full_path_string());
                    ui.separator();
                    detail(ui, "Type", if entry.is_file() { "File" } else { "Folder" });
                    detail(
                        ui,
                        "Size",
                        &if entry.is_file() {
                            format_size(entry.meta.size)
                        } else {
                            "Unavailable".into()
                        },
                    );
                    detail(ui, "Created", &format_date(entry.meta.created_at));
                    detail(ui, "Modified", &format_date(entry.meta.modified_at));
                });
                ui.add_space(12.0);
                ui.horizontal(|ui| {
                    if ui.button("Open").clicked() {
                        if entry.is_file() {
                            ActionToPerform::SystemOpen(entry.full_path_string().into()).schedule();
                        } else {
                            TabAction::ChangePaths(entry.get_path().into())
                                .schedule_tab(view.tab_id);
                        }
                    }
                    if ui.button("Copy Path").clicked() {
                        ui.ctx().copy_text(entry.full_path_string());
                    }
                });
            });
    }
    pub(crate) fn panel_overlays(&mut self, ctx: &Context) {
        if ctx.content_rect().width() >= 800.0 {
            self.sidebar_overlay = false;
        }
        if ctx.content_rect().width() >= 1000.0 {
            self.inspector_overlay = false;
        }
        if self.display_modal.is_some() {
            return;
        }
        if self.sidebar_overlay {
            let modal = egui::Modal::new("sidebar_overlay".into()).show(ctx, |ui| {
                ui.set_width(260.0);
                if ui.button("Close sidebar").clicked() {
                    self.sidebar_overlay = false;
                }
                ui.add_space(8.0);
                ui.set_max_height(ctx.content_rect().height() - 80.0);
                ui.vertical(|ui| self.locations_ui(ui));
            });
            if modal.should_close() {
                self.sidebar_overlay = false;
            }
        } else if self.inspector_overlay {
            let modal = egui::Modal::new("inspector_overlay".into()).show(ctx, |ui| {
                ui.set_width(300.0);
                if ui.button("Close inspector").clicked() {
                    self.inspector_overlay = false;
                }
                ui.add_space(8.0);
                ui.set_max_height(ctx.content_rect().height() - 80.0);
                ui.vertical(|ui| self.inspector_ui(ui));
            });
            if modal.should_close() {
                self.inspector_overlay = false;
            }
        }
    }
}
fn detail(ui: &mut Ui, label: &str, value: &str) {
    ui.vertical(|ui| {
        ui.label(
            egui::RichText::new(label)
                .small()
                .color(Palette::of(ui).secondary),
        );
        ui.add(egui::Label::new(value).wrap());
    });
    ui.add_space(4.0);
}
fn format_date(timestamp: TimestampSeconds) -> String {
    if *timestamp == 0 {
        return "Unavailable".into();
    }
    chrono::DateTime::from_timestamp(i64::from(*timestamp), 0).map_or_else(
        || "Unavailable".into(),
        |date| date.format("%Y-%m-%d %H:%M UTC").to_string(),
    )
}
fn format_size(size: u64) -> String {
    if size < 1024 {
        return format!("{size} bytes");
    }
    let (unit, divisor) = if size < 1024 * 1024 {
        ("KB", 1024.0)
    } else if size < 1024 * 1024 * 1024 {
        ("MB", 1024.0 * 1024.0)
    } else {
        ("GB", 1024.0 * 1024.0 * 1024.0)
    };
    format!("{:.1} {unit}", size as f64 / divisor)
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn absent_dates_are_not_shown_as_epoch() {
        assert_eq!(format_date(TimestampSeconds::default()), "Unavailable");
    }
    #[test]
    fn size_is_readable() {
        assert_eq!(format_size(512), "512 bytes");
        assert_eq!(format_size(2048), "2.0 KB");
    }
}
