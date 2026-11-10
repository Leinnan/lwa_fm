//! The shared core (`dirfleet_core`) running inside the egui app, with its
//! Rust effect handlers, as every shell runs it. Handler threads mark the view
//! model stale and wake egui; each frame takes a fresh view model only then.
//!
//! The egui app moves onto it in steps (`docs/crux-migration/workspace.md`).
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, OnceLock, Weak};

use crux_core::Core;
use crux_core::effects::{EffectRouter, Routes};
use dirfleet_core::app::{DirFleet, Effect, Event, ViewModel};
use dirfleet_effects::Handlers;

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
    repaint: Arc<Repaint>,
    router: Weak<EffectRouter<DirFleet, Self>>,
}

impl Routes<DirFleet> for EguiRoutes {
    fn new(router: Weak<EffectRouter<DirFleet, Self>>) -> Self {
        Self {
            repaint: Arc::default(),
            router,
        }
    }
}

/// The core, and its view model as of the last render.
pub struct CoreLink {
    router: Arc<EffectRouter<DirFleet, EguiRoutes>>,
    view: ViewModel,
}

impl Default for CoreLink {
    fn default() -> Self {
        let router = EffectRouter::new(Core::<DirFleet>::new(), |routes: EguiRoutes| {
            let handlers = Handlers::new(&routes.router, None);
            move |effect| {
                // Other shell effects come with the steps that need them.
                if let Some(Effect::Render(_)) = handlers.handle(effect) {
                    routes.repaint.request();
                }
            }
        });
        Self {
            router,
            view: ViewModel::default(),
        }
    }
}

#[cfg_attr(
    not(test),
    expect(dead_code, reason = "the app reads the core from the next step on")
)]
impl CoreLink {
    /// Called at the start of every frame: wakes egui from now on, and takes
    /// the new view model when the core rendered since the last frame.
    pub fn begin_frame(&mut self, ctx: &egui::Context) {
        let repaint = &self.router.routes.repaint;
        let _ = repaint.ctx.set(ctx.clone());
        if repaint.stale.swap(false, Ordering::SeqCst) {
            self.view = self.router.view();
        }
    }

    /// Sends `event` to the core; its results arrive by a later frame.
    pub fn send(&self, event: Event) {
        self.router.update(event);
    }

    /// The view model as of the last render.
    pub const fn view(&self) -> &ViewModel {
        &self.view
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_core_runs_inside_egui_and_its_renders_reach_the_frame() {
        let ctx = egui::Context::default();
        let mut core = CoreLink::default();
        core.begin_frame(&ctx);
        core.send(Event::NewWindow { path: None });
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while core.view().windows.is_empty() {
            assert!(
                std::time::Instant::now() < deadline,
                "no render reached the frame"
            );
            std::thread::sleep(std::time::Duration::from_millis(5));
            core.begin_frame(&ctx);
        }
        assert_eq!(core.view().windows.len(), 1);
    }
}
