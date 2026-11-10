//! The spike's egui shell: the shared core and Rust handlers behind crux's
//! `EffectRouter`, drawn with egui. `cargo run --example core_spike [folder]`
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, OnceLock, Weak};

use crux_core::Core;
use crux_core::effects::{EffectRouter, Routes};
use dirfleet_core::app::{DirFleet, Event, TabViewModel};
use dirfleet_effects::Handlers;
use eframe::egui;

/// Marks the view model stale and wakes egui; called from handler threads.
#[derive(Default)]
struct Repaint {
    ctx: OnceLock<egui::Context>,
    stale: AtomicBool,
}

impl Repaint {
    fn request(&self) {
        self.stale.store(true, Ordering::SeqCst);
        if let Some(ctx) = self.ctx.get() {
            ctx.request_repaint();
        }
    }
}

#[derive(Clone)]
struct EguiRoutes {
    handlers: Arc<Handlers>,
    repaint: Arc<Repaint>,
}

impl Routes<DirFleet> for EguiRoutes {
    fn new(router: Weak<EffectRouter<DirFleet, Self>>) -> Self {
        Self {
            handlers: Arc::new(Handlers::new(&router, None)),
            repaint: Arc::default(),
        }
    }
}

struct Spike {
    router: Arc<EffectRouter<DirFleet, EguiRoutes>>,
    /// The one window's visible tab: this shell opens one window, never more.
    view: TabViewModel,
    path_text: String,
    shown: (u32, u32),
}

impl eframe::App for Spike {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let _ = self.router.routes.repaint.ctx.set(ui.ctx().clone());
        if self
            .router
            .routes
            .repaint
            .stale
            .swap(false, Ordering::SeqCst)
        {
            if let Some(window) = self.router.view().windows.into_iter().next() {
                self.view = window.active;
            }
            if !self.view.path.is_empty() {
                self.path_text.clone_from(&self.view.path);
            }
        }
        let view = &self.view;
        let tab = view.id;
        egui::Panel::top("path").show(ui, |ui| {
            ui.horizontal(|ui| {
                if ui
                    .add_enabled(view.nav.back, egui::Button::new("⬅"))
                    .clicked()
                {
                    self.router.update(Event::Back { tab });
                }
                let edit = ui.text_edit_singleline(&mut self.path_text);
                if edit.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                    self.router.update(Event::Navigate {
                        tab,
                        path: self.path_text.clone(),
                    });
                }
                if view.loading {
                    ui.spinner();
                }
            });
        });
        egui::CentralPanel::default().show(ui, |ui| {
            if let Some(error) = &view.error {
                ui.colored_label(egui::Color32::RED, error);
            }
            let row_height = ui.text_style_height(&egui::TextStyle::Body) + 4.0;
            let total = view.total_rows as usize;
            let mut open = None;
            let mut range_seen = None;
            egui::ScrollArea::vertical().show_rows(ui, row_height, total, |ui, range| {
                range_seen = Some((range.start as u32, range.len() as u32));
                for position in range {
                    let position = position as u32;
                    let Some(row) = position
                        .checked_sub(view.first_row)
                        .and_then(|offset| view.rows.get(offset as usize))
                    else {
                        ui.label("");
                        continue;
                    };
                    ui.horizontal(|ui| {
                        let icon = if row.is_dir { "📁" } else { "📄" };
                        let name = ui.add(
                            egui::Label::new(format!("{icon} {}", row.name))
                                .sense(egui::Sense::click()),
                        );
                        if name.double_clicked() {
                            open = Some(position);
                        }
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            // Right to left: the last column first.
                            for cell in row.cells.iter().rev() {
                                ui.label(cell);
                            }
                        });
                    });
                }
            });
            if let Some((first_row, rows)) = range_seen
                && (first_row, rows) != self.shown
            {
                self.shown = (first_row, rows);
                self.router.update(Event::ViewportChanged {
                    tab,
                    first_row,
                    rows,
                });
            }
            if let Some(position) = open {
                self.router.update(Event::OpenRow { tab, row: position });
            }
        });
    }
}

fn main() -> eframe::Result {
    let start = std::env::args()
        .nth(1)
        .or_else(|| std::env::var("HOME").ok())
        .unwrap_or_else(|| "/".into());
    let router = EffectRouter::new(Core::<DirFleet>::new(), |routes: EguiRoutes| {
        move |effect| {
            if routes.handlers.handle(effect).is_some() {
                routes.repaint.request();
            }
        }
    });
    router.update(Event::NewWindow { path: Some(start) });
    eframe::run_native(
        "DirFleet core spike",
        eframe::NativeOptions::default(),
        Box::new(|_| {
            Ok(Box::new(Spike {
                router,
                view: TabViewModel::default(),
                path_text: String::new(),
                shown: (0, 0),
            }))
        }),
    )
}
