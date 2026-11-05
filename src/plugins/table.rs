//! `dirfleet.app.show_table`: a list of a plugin's results, sorted by clicking a
//! column's header, drawn in the action's dialog (`panel`). Like forms, it is data:
//! no Lua runs while it is open. Rows stand for folders, so their payload is a path.

use egui_mev_data::table as shared;
use std::path::PathBuf;

pub use shared::{RowStatus, SizeFilter, SortOrder};

pub type RowClick = shared::RowClick<PathBuf>;
pub type RowUpdate = shared::RowUpdate<PathBuf>;
pub type TableSpec = shared::TableSpec<PathBuf>;
pub type TableView = shared::TableView<PathBuf>;

/// A view of a plugin's table, whose menus talk about folders.
pub fn view(spec: TableSpec) -> TableView {
    TableView::new(spec)
        .labels(shared::TableLabels {
            open: "Open in new tab".into(),
            refresh: "Measure again".into(),
            copy: "Copy path".into(),
            job_running: "Stop the scan first".into(),
            row_busy: "Wait for this folder to finish".into(),
        })
        .payload_text(|path| path.display().to_string())
}
