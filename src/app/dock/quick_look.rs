//! Quick Look: Space shows a large preview of the focused item over the view.
//! Arrow keys move to the next item; Space or Esc close it.

use super::{MyTabViewer, TabData};
use crate::app::assets::{Preview, PreviewIntent};
use crate::app::ui::{self, Palette};
use egui::{Context, Vec2};

impl MyTabViewer<'_> {
    pub(super) fn quick_look_ui(&mut self, ctx: &Context, tab: &mut TabData) {
        if !tab.quick_look || self.active_tab != tab.id {
            return;
        }
        let Some(row) = tab
            .selection
            .cursor()
            .or_else(|| tab.selection.rows().next())
        else {
            tab.quick_look = false;
            return;
        };
        let Some(entry) = tab.entry_at(row) else {
            tab.quick_look = false;
            return;
        };
        let screen = ctx.content_rect();
        let target = Vec2::new(screen.width() * 0.6, screen.height() * 0.6);
        let preview =
            self.assets
                .request_preview_at_size(ctx, &entry, PreviewIntent::Focused, target);
        let name = entry.file_name.to_owned();
        let path = entry.full_path_string();
        let mut close = false;
        egui::Area::new(egui::Id::unique("quick_look").with(tab.id))
            .order(egui::Order::Foreground)
            .anchor(egui::Align2::CENTER_CENTER, Vec2::ZERO)
            .show(ctx, |ui| {
                egui::Frame::popup(ui.style())
                    .fill(Palette::of(ui).card_bg)
                    .inner_margin(16)
                    .show(ui, |ui| {
                        ui.set_max_width(target.x);
                        ui.horizontal(|ui| {
                            ui.heading(&name);
                            ui.with_layout(
                                egui::Layout::right_to_left(egui::Align::Center),
                                |ui| {
                                    close = ui::icon_button(
                                        ui,
                                        lucide_icons::Icon::X,
                                        "Close (Esc)",
                                        false,
                                    )
                                    .clicked();
                                },
                            );
                        });
                        ui.weak(&path);
                        ui.add_space(8.0);
                        match preview {
                            Preview::Ready(texture) => {
                                ui.add(
                                    egui::Image::new(&texture)
                                        .maintain_aspect_ratio(true)
                                        .max_size(target),
                                );
                            }
                            Preview::Pending => {
                                ui.horizontal(|ui| {
                                    ui.spinner();
                                    ui.label("Loading preview…");
                                });
                                ctx.request_repaint_after(std::time::Duration::from_millis(100));
                            }
                            Preview::Unavailable { reason, .. } => {
                                ui.label("No preview for this item.");
                                ui.weak(reason);
                            }
                            Preview::Fallback => {
                                ui.label(if entry_is_dir(tab, row) {
                                    "This is a folder. Press Enter to open it."
                                } else {
                                    "No preview for this type of file."
                                });
                            }
                        }
                        ui.add_space(8.0);
                        ui.weak("↑ ↓ ← → next item · Space or Esc close · Enter open");
                    });
            });
        if close {
            tab.quick_look = false;
        }
    }
}

fn entry_is_dir(tab: &TabData, row: usize) -> bool {
    tab.entry_at(row).is_some_and(|entry| !entry.is_file())
}
