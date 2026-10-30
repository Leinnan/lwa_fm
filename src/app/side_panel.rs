use super::{
    App,
    settings::SidebarSection,
    ui::{Palette, PanelLayout},
};
use crate::{helper::DataHolder, locations::Locations};
use egui::Ui;
use std::path::{Path, PathBuf};

/// Drag payload for reordering the sidebar sections by their headers.
#[derive(Clone, Copy)]
struct DraggedSidebarSection(SidebarSection);

/// A change to the sidebar sections asked for while drawing them.
enum SectionChange {
    Toggle(SidebarSection),
    /// Moves the first section to where the second one is, so hidden
    /// sections keep their place in the order.
    Move(SidebarSection, SidebarSection),
    CollapseAll(bool),
}

impl App {
    pub(crate) fn locations_ui(&mut self, ui: &mut Ui) {
        let active = self.tabs.get_current_path();
        let active_saved = self
            .tabs
            .get_current_tab()
            .and_then(|tab| tab.search.as_ref())
            .and_then(|search| search.saved_from);
        let favorites = ui.data_get_persisted::<Locations>().unwrap_or_default();
        let saved = super::saved_searches::load_saved(ui.ctx());
        let recent = self.recent_folders(active.as_deref());
        // Only sections with something in them are shown.
        let shown: Vec<SidebarSection> = self
            .settings
            .sidebar_order()
            .into_iter()
            .filter(|section| match section {
                SidebarSection::Favorites => !favorites.locations.is_empty(),
                SidebarSection::SavedSearches => !saved.searches.is_empty(),
                SidebarSection::Recent => !recent.is_empty(),
                SidebarSection::Locations => {
                    !self.user_locations.locations.is_empty() || self.settings.show_trash
                }
                SidebarSection::Drives => self.drives.visible().next().is_some(),
            })
            .collect();
        let mut change = None;
        egui::ScrollArea::vertical().show(ui, |ui| {
            for (index, &section) in shown.iter().enumerate() {
                let collapsed = self.settings.is_section_collapsed(section);
                if let Some(next) =
                    Self::sidebar_section_header(ui, section, collapsed, index, &shown)
                {
                    change = Some(next);
                }
                if collapsed {
                    continue;
                }
                match section {
                    SidebarSection::Favorites => {
                        favorites.draw_source_list(ui, true, active.as_deref());
                    }
                    SidebarSection::SavedSearches => {
                        Self::saved_searches_sidebar(ui, &saved, active_saved);
                    }
                    SidebarSection::Recent => Self::recent_folders_ui(ui, &recent, &favorites),
                    SidebarSection::Locations => {
                        self.user_locations
                            .draw_source_list(ui, false, active.as_deref());
                        if self.settings.show_trash {
                            let in_trash = self
                                .tabs
                                .get_current_tab()
                                .is_some_and(|tab| tab.current_path.is_trash());
                            Self::trash_sidebar_row(ui, in_trash);
                        }
                    }
                    SidebarSection::Drives => {
                        self.drives_sidebar(ui, active.as_deref(), &favorites);
                    }
                }
            }
        });
        match change {
            Some(SectionChange::Toggle(section)) => {
                let collapsed = self.settings.is_section_collapsed(section);
                self.settings.set_section_collapsed(section, !collapsed);
            }
            Some(SectionChange::Move(section, target)) => {
                if let Some(to) = self
                    .settings
                    .sidebar_order()
                    .iter()
                    .position(|s| *s == target)
                {
                    self.settings.move_sidebar_section(section, to);
                }
            }
            Some(SectionChange::CollapseAll(collapsed)) => {
                for section in SidebarSection::ALL {
                    self.settings.set_section_collapsed(section, collapsed);
                }
            }
            None => {}
        }
    }

    /// Draws a section's header: click to collapse or expand, drag onto
    /// another header to move it there, or use its context menu.
    fn sidebar_section_header(
        ui: &mut Ui,
        section: SidebarSection,
        collapsed: bool,
        index: usize,
        shown: &[SidebarSection],
    ) -> Option<SectionChange> {
        let mut change = None;
        let response = super::ui::section_header(ui, section.label(), collapsed);
        if response.clicked() {
            change = Some(SectionChange::Toggle(section));
        }
        response.dnd_set_drag_payload(DraggedSidebarSection(section));
        if let Some(dragged) = response.dnd_hover_payload::<DraggedSidebarSection>()
            && dragged.0 != section
        {
            ui.painter().hline(
                response.rect.x_range(),
                response.rect.top(),
                egui::Stroke::new(2.0, Palette::of(ui).accent),
            );
        }
        if let Some(dragged) = response.dnd_release_payload::<DraggedSidebarSection>()
            && dragged.0 != section
        {
            change = Some(SectionChange::Move(dragged.0, section));
        }
        response.context_menu(|ui| {
            let label = if collapsed { "Expand" } else { "Collapse" };
            if ui.button(label).clicked() {
                change = Some(SectionChange::Toggle(section));
            }
            ui.separator();
            if ui
                .add_enabled(index > 0, egui::Button::new("Move up"))
                .clicked()
            {
                change = Some(SectionChange::Move(section, shown[index - 1]));
            }
            if ui
                .add_enabled(index + 1 < shown.len(), egui::Button::new("Move down"))
                .clicked()
            {
                change = Some(SectionChange::Move(section, shown[index + 1]));
            }
            ui.separator();
            if ui.button("Collapse all").clicked() {
                change = Some(SectionChange::CollapseAll(true));
            }
            if ui.button("Expand all").clicked() {
                change = Some(SectionChange::CollapseAll(false));
            }
        });
        change
    }

    pub(crate) fn left_side_panel(&mut self, root: &mut Ui) {
        let ctx = &root.ctx().clone();
        let layout = PanelLayout::new(
            ctx.content_rect().width(),
            self.settings.sidebar_visible,
            self.settings.inspector_visible,
        );
        if !layout.sidebar {
            return;
        }
        let panel = egui::Panel::left("leftPanel")
            .default_size(self.settings.sidebar_width.clamp(160.0, 280.0))
            .size_range(160.0..=280.0)
            .resizable(true)
            .frame(
                egui::Frame::new()
                    .fill(Palette::for_context(ctx, ctx.global_style().visuals.dark_mode).sidebar)
                    .inner_margin(8),
            )
            .show(root, |ui| self.locations_ui(ui));
        self.settings.sidebar_width = panel.response.rect.width().clamp(160.0, 280.0);
    }

    /// The last few folders opened, other than the active one; empty when
    /// the "Recent" section is turned off.
    fn recent_folders(&self, active: Option<&Path>) -> Vec<PathBuf> {
        if !self.settings.show_recent_folders {
            return Vec::new();
        }
        self.recent_folders
            .iter()
            .filter(|folder| Some(folder.as_path()) != active)
            .take(5)
            .cloned()
            .collect()
    }

    /// The rows of the "Recent" section.
    fn recent_folders_ui(ui: &mut Ui, recent: &[PathBuf], favorites: &Locations) {
        for folder in recent {
            let name = folder.file_name().map_or_else(
                || folder.display().to_string(),
                |n| n.to_string_lossy().into_owned(),
            );
            let response = super::ui::nav_row(
                ui,
                lucide_icons::Icon::Clock,
                &name,
                false,
                egui::Sense::click(),
            )
            .on_hover_text(folder.display().to_string());
            let new_tab = response.middle_clicked()
                || (response.clicked() && ui.input(|i| i.modifiers.command));
            if (response.clicked() || new_tab)
                && let Some(action) =
                    super::ActionToPerform::path_from_str(folder.to_string_lossy(), new_tab)
            {
                action.schedule();
            }
            crate::app::file_ops::folder_drop_target(ui, &response, folder);
            response.context_menu(|ui| {
                super::menus::folder_actions(ui, folder, favorites, true);
            });
        }
    }
}
