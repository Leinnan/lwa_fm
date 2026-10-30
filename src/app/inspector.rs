use super::{
    App,
    assets::{Preview, PreviewIntent},
    commands::{ActionToPerform, TabAction},
    ui::{self, Palette},
};
use crate::app::dock::TabId;
use crate::data::{files::DirEntry, time::TimestampSeconds};
use egui::{Context, Ui, Vec2};
use std::path::PathBuf;

#[derive(Clone, Debug)]
pub struct InspectorView {
    pub tab_id: TabId,
    pub paths: Vec<PathBuf>,
    pub entry: Option<DirEntry>,
}
impl App {
    pub(crate) fn inspector_view(&mut self, _ctx: &Context) -> Option<InspectorView> {
        let tab = self.tabs.get_current_tab()?;
        let entries: Vec<_> = tab
            .selection
            .rows()
            .filter_map(|index| tab.entry_at(index).map(|entry| entry.to_entry()))
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
                    self.selection_summary_ui(ui, view.tab_id, view.paths.len());
                    self.plugin_inspector_ui(ui);
                    return;
                }
                let Some(entry) = view.entry else {
                    return;
                };
                ui.add_space(8.0);
                let width = ui.available_width();
                ui::card(ui).show(ui, |ui| {
                    let target = Vec2::splat((width - 24.0).clamp(96.0, 300.0));
                    let preview = self.assets.request_preview_at_size(
                        ui.ctx(),
                        &entry.entry_ref(),
                        PreviewIntent::Focused,
                        target,
                    );
                    render_preview(ui, &mut self.assets, &entry, preview, target);
                });
                ui::section(ui, "Information");
                ui::card(ui).show(ui, |ui| {
                    ui.add(egui::Label::new(egui::RichText::new(&entry.file_name).strong()).wrap());
                    ui.add(egui::Label::new(entry.full_path_string()).wrap())
                        .on_hover_text(entry.full_path_string());
                    ui.separator();
                    detail(
                        ui,
                        "Type",
                        &crate::app::dock::kind_label(&entry.file_name, entry.meta.entry_type),
                    );
                    if entry.meta.entry_type == crate::data::files::EntryType::File {
                        detail(ui, "Size", &format_size(entry.meta.size));
                    } else {
                        folder_size_row(ui, &entry.get_path());
                    }
                    detail(ui, "Created", &format_date(entry.meta.created_at));
                    detail(ui, "Modified", &format_date(entry.meta.modified_at));
                    let record = crate::app::open_history::of_entry(&entry.entry_ref());
                    let opened =
                        crate::app::open_history::last_opened(record, entry.meta.accessed_at);
                    detail(ui, "Opened", &format_date(opened));
                    if let Some(record) = record {
                        let times = if record.count == 1 { "time" } else { "times" };
                        detail(ui, "Opened here", &format!("{} {times}", record.count));
                    }
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
                    if ui.button("Copy as path").clicked() {
                        ui.ctx().copy_text(entry.full_path_string());
                    }
                });
                self.plugin_inspector_ui(ui);
            });
    }
    pub(crate) fn panel_overlays(&mut self, ctx: &Context) {
        if ctx.content_rect().width() >= 800.0 {
            self.tabs.window_mut().sidebar_overlay = false;
        }
        if ctx.content_rect().width() >= 1000.0 {
            self.tabs.window_mut().inspector_overlay = false;
        }
        if self.display_modal.is_some() {
            return;
        }
        if self.tabs.window().sidebar_overlay {
            let modal = egui::Modal::new("sidebar_overlay".into()).show(ctx, |ui| {
                ui.vertical(|ui| {
                    ui.set_width(260.0);
                    ui.set_max_height(ctx.content_rect().height() - 80.0);
                    if ui.button("Close sidebar").clicked() {
                        self.tabs.window_mut().sidebar_overlay = false;
                    }
                    ui.add_space(8.0);
                    self.locations_ui(ui);
                });
            });
            if modal.should_close() {
                self.tabs.window_mut().sidebar_overlay = false;
            }
        } else if self.tabs.window().inspector_overlay {
            let modal = egui::Modal::new("inspector_overlay".into()).show(ctx, |ui| {
                ui.vertical(|ui| {
                    ui.set_width(300.0);
                    ui.set_max_height(ctx.content_rect().height() - 80.0);
                    if ui.button("Close inspector").clicked() {
                        self.tabs.window_mut().inspector_overlay = false;
                    }
                    ui.add_space(8.0);
                    self.inspector_ui(ui);
                });
            });
            if modal.should_close() {
                self.tabs.window_mut().inspector_overlay = false;
            }
        }
    }
}
pub(super) fn render_preview(
    ui: &mut Ui,
    assets: &mut super::assets::AssetManager,
    entry: &DirEntry,
    preview: Preview,
    target: Vec2,
) {
    match preview {
        Preview::Ready(texture) => {
            ui.add(
                egui::Image::new(&texture)
                    .maintain_aspect_ratio(true)
                    .max_size(target),
            );
        }
        Preview::Pending => {
            ui.spinner();
            ui.weak("Loading preview…");
        }
        Preview::Unavailable {
            reason,
            retry_after,
        } => {
            ui.weak("Preview unavailable");
            ui.small(reason);
            if let Some(delay) = retry_after {
                ui.small(format!("Retrying in {} seconds", delay.as_secs().max(1)));
            }
        }
        Preview::Fallback => {
            if let Some(texture) = assets.request_entry_texture(&entry.entry_ref()) {
                ui.add(egui::Image::new(&texture).fit_to_exact_size(Vec2::splat(64.0)));
            }
            ui.weak(if entry.is_file() {
                "No preview available"
            } else {
                "Folder"
            });
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
pub fn format_date(timestamp: TimestampSeconds) -> String {
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
/// The result of adding up a folder's size in the background.
type FolderSize = std::sync::Arc<std::sync::Mutex<Option<u64>>>;

/// "Size" for a folder: computed on request, since walking a large tree is slow.
fn folder_size_row(ui: &mut Ui, path: &std::path::Path) {
    let id = egui::Id::new(("folder_size", path));
    let state: Option<FolderSize> = ui.data(|d| d.get_temp(id));
    match state {
        None => {
            ui.horizontal(|ui| {
                ui.weak("Size");
                if ui.small_button("Calculate").clicked() {
                    let result: FolderSize = std::sync::Arc::default();
                    let writer = std::sync::Arc::clone(&result);
                    let root = path.to_path_buf();
                    let ctx = ui.ctx().clone();
                    std::thread::spawn(move || {
                        let total: u64 = walkdir::WalkDir::new(root)
                            .into_iter()
                            .filter_map(Result::ok)
                            .filter_map(|entry| entry.metadata().ok())
                            .filter(std::fs::Metadata::is_file)
                            .map(|meta| meta.len())
                            .sum();
                        if let Ok(mut slot) = writer.lock() {
                            *slot = Some(total);
                        }
                        ctx.request_repaint();
                    });
                    ui.data_mut(|d| d.insert_temp(id, result));
                }
            });
        }
        Some(result) => {
            let total = result.lock().ok().and_then(|slot| *slot);
            match total {
                Some(bytes) => detail(ui, "Size", &format_size(bytes)),
                None => {
                    ui.horizontal(|ui| {
                        ui.weak("Size");
                        ui.spinner();
                    });
                }
            }
        }
    }
}

impl App {
    /// What the inspector shows for several selected items: counts, total
    /// size, a breakdown by type and the actions that work on all of them.
    fn selection_summary_ui(&mut self, ui: &mut Ui, tab_id: TabId, count: usize) {
        use crate::app::keymap::{self, Command};
        use crate::app::search::filter::kind_of;
        use crate::data::files::EntryType;
        let Some(tab) = self.tabs.get_tab_by_id(tab_id) else {
            return;
        };
        let mut folders = 0;
        let mut bytes = 0_u64;
        let mut kinds = std::collections::BTreeMap::<&'static str, usize>::new();
        for row in tab.selection.rows() {
            let Some(&index) = tab.visible_entries.get(row) else {
                continue;
            };
            let meta = tab.listing.meta(index as usize);
            let is_dir = meta.entry_type == EntryType::Directory;
            if is_dir {
                folders += 1;
            } else if meta.entry_type == EntryType::File {
                bytes += meta.size;
            }
            let kind = kind_of(tab.listing.name(index as usize), is_dir)
                .map_or("Other", crate::app::search::FileKind::label);
            *kinds.entry(kind).or_default() += 1;
        }
        ui::section(ui, "Selection");
        ui::card(ui).show(ui, |ui| {
            ui.strong(format!("{count} items selected"));
            ui.label(format!("{} folders, {} files", folders, count - folders));
            if bytes > 0 {
                ui.label(format!(
                    "Files total {}",
                    crate::helper::format_bytes_simple(bytes)
                ));
            }
            ui.add_space(4.0);
            for (kind, n) in &kinds {
                ui.weak(format!("{kind}: {n}"));
            }
        });
        ui::section(ui, "Actions");
        ui.horizontal_wrapped(|ui| {
            for command in [
                Command::Copy,
                Command::Cut,
                Command::Duplicate,
                Command::Rename,
                Command::Delete,
            ] {
                if ui
                    .button(command.label())
                    .on_hover_text(keymap::tooltip(ui.ctx(), command))
                    .clicked()
                {
                    crate::app::commands::ActionToPerform::RunCommand(command).schedule();
                }
            }
        });
    }
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
