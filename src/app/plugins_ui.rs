//! Where plugins meet the app: starting their actions with the active tab's
//! selection, carrying out what they ask for, and drawing their bar.

use super::App;
use super::command_palette::ValidAction;
use super::commands::{ActionToPerform, ModalWindow, TabAction, TabTarget};
use crate::plugins::{AppRequest, CallContext, NotifyLevel, PluginMsg};
use crate::toast;
use std::borrow::Cow;

impl App {
    /// The active tab's folder and selected items, for plugin handlers.
    pub(crate) fn plugin_context(&mut self) -> CallContext {
        let Some(tab) = self.tabs.get_current_tab() else {
            return CallContext::default();
        };
        if tab.current_path.is_trash() {
            return CallContext::default();
        }
        CallContext {
            dir: tab.current_path.single_path(),
            targets: crate::plugins::host_targets(
                tab.selection.rows().filter_map(|row| tab.entry_at(row)),
            ),
            tab: Some(tab.id.number()),
            values: None,
        }
    }

    /// Starts a plugin action; menus pass their own targets, shortcuts and
    /// the palette use the active tab's.
    pub(crate) fn run_plugin_action(&mut self, action: &str, mut context: CallContext) {
        let current = self.plugin_context();
        if context.targets.is_empty() && context.dir.is_none() {
            context = current;
        } else {
            context.tab = current.tab;
        }
        if self
            .plugins
            .start_action(action, context, &self.settings.plugins)
        {
            self.display_modal = Some(ModalWindow::PluginForm);
        }
    }

    pub(crate) fn handle_plugin_message(&mut self, ctx: &egui::Context, message: PluginMsg) {
        match message {
            PluginMsg::App(request) => self.plugin_request(ctx, request),
            message => self.plugins.handle(message, &mut self.settings.plugins),
        }
    }

    fn plugin_request(&mut self, ctx: &egui::Context, request: AppRequest) {
        match request {
            AppRequest::Open(path) => {
                ActionToPerform::TabAction(
                    TabTarget::ActiveTab,
                    TabAction::ChangePaths(path.into()),
                )
                .schedule();
            }
            AppRequest::OpenInNewTab(path) => ActionToPerform::NewTab(path).schedule(),
            AppRequest::Refresh => {
                ActionToPerform::TabAction(TabTarget::AllTabs, TabAction::RequestFilesRefresh)
                    .schedule();
            }
            AppRequest::Select(paths) => {
                if let Some(tab) = self.tabs.get_current_tab() {
                    tab.pending_select = Some(super::file_ops::PendingSelect {
                        paths,
                        rename: false,
                        since: std::time::Instant::now(),
                    });
                }
            }
            AppRequest::SystemOpen(path) => {
                ActionToPerform::SystemOpen(path.to_string_lossy().into_owned().into()).schedule();
            }
            AppRequest::OpenTerminal(dir) => ActionToPerform::OpenInTerminal(dir).schedule(),
            AppRequest::FileOp(op) => ActionToPerform::FileOp(op).schedule(),
            AppRequest::Clipboard(text) => ctx.copy_text(text),
            AppRequest::Notify { level, text } => match level {
                NotifyLevel::Info => {
                    toast!(Info, "{text}");
                }
                NotifyLevel::Success => {
                    toast!(Success, "{text}");
                }
                NotifyLevel::Warning => {
                    toast!(Warning, "{text}");
                }
                NotifyLevel::Error => {
                    toast!(Error, "{text}");
                }
            },
        }
    }

    /// Once a frame: the host's housekeeping and `selection_changed`.
    pub(crate) fn poll_plugins(&mut self) {
        self.plugins.poll(&self.settings.plugins);
        if !self.plugins.wants("selection_changed") {
            return;
        }
        let key = {
            use std::hash::{Hash, Hasher};
            let Some(tab) = self.tabs.get_current_tab() else {
                return;
            };
            let mut hasher = std::hash::DefaultHasher::new();
            tab.id.hash(&mut hasher);
            tab.current_path.hash(&mut hasher);
            for row in tab.selection.rows() {
                row.hash(&mut hasher);
            }
            hasher.finish()
        };
        if self.plugins.selection_settled(key) {
            let context = self.plugin_context();
            self.plugins
                .emit("selection_changed", &context, &serde_json::Value::Null);
        }
    }

    /// Plugin actions for the command palette, for the active selection.
    pub(crate) fn plugin_palette_commands(&mut self, ctx: &egui::Context) -> Vec<ValidAction> {
        let context = self.plugin_context();
        let registry = crate::plugins::registry();
        registry
            .actions
            .iter()
            .filter(|action| {
                action.def.palette
                    && crate::plugins::applies(action, &context.targets, context.dir.as_deref())
            })
            .map(|action| ValidAction {
                action: ActionToPerform::RunPluginAction {
                    action: action.full_id.clone(),
                    context: CallContext::default(),
                },
                name: Cow::Owned(action.def.title.clone()),
                shortcut: action
                    .shortcut
                    .as_ref()
                    .map(|shortcut| ctx.format_shortcut(shortcut)),
            })
            .collect()
    }

    /// The plugins' bar under the file view, for the active tab's folder.
    pub(crate) fn plugin_bar(&mut self, root: &mut egui::Ui) {
        let Some(dir) = self
            .tabs
            .get_current_tab()
            .filter(|tab| !tab.current_path.is_trash())
            .and_then(|tab| tab.current_path.single_path())
        else {
            return;
        };
        let views = self.plugins.bar_for(&dir);
        if views.is_empty() {
            return;
        }
        let ctx = root.ctx().clone();
        let mut clicked = None;
        egui::Panel::bottom("pluginBar")
            .resizable(false)
            .frame(
                egui::Frame::new()
                    .fill(
                        super::ui::Palette::for_context(&ctx, ctx.global_style().visuals.dark_mode)
                            .toolbar,
                    )
                    .inner_margin(egui::Margin::symmetric(12, 3)),
            )
            .show(root, |ui| {
                clicked = crate::plugins::ui::bar_ui(ui, &views);
            });
        if let Some(action) = clicked {
            ActionToPerform::RunPluginAction {
                action,
                context: CallContext {
                    dir: Some(dir),
                    ..Default::default()
                },
            }
            .schedule();
        }
    }

    /// The plugins' sections in the inspector, for the selection.
    pub(crate) fn plugin_inspector_ui(&mut self, ui: &mut egui::Ui) {
        let context = self.plugin_context();
        if context.targets.is_empty() {
            return;
        }
        let views = self.plugins.inspector_for(&context);
        crate::plugins::ui::inspector_ui(ui, &views);
    }
}
