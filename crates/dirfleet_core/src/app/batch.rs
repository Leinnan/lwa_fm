//! Batch rename, as egui's: several selected items renamed at once, by
//! replacing text in their names or from a pattern with `{name}`, `{n}` and
//! `{ext}`, with a live preview that flags names that can't be used. It runs
//! as one undoable step.
use std::path::{Path, PathBuf};

use crux_core::Command;
use crux_core::render::render;
use facet::Facet;
use serde::{Deserialize, Serialize};

use super::{DirFleet, Effect, Event, Model, Tab, tab_id};
use crate::fileops::{FileOp, Origin, validate_name};
use crate::listing::EntryType;

#[derive(Facet, Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq, Default)]
#[repr(C)]
pub enum BatchMode {
    /// Replace text in every name.
    #[default]
    Replace,
    /// Build names from a pattern with `{name}`, `{n}` and `{ext}`.
    Pattern,
}

/// The batch rename's fields, as the shell's dialog edits them.
#[derive(Facet, Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct BatchFields {
    pub mode: BatchMode,
    pub find: String,
    pub replace: String,
    pub pattern: String,
    /// The first `{n}`.
    pub start: u32,
}

impl Default for BatchFields {
    fn default() -> Self {
        Self {
            mode: BatchMode::Replace,
            find: String::new(),
            replace: String::new(),
            pattern: "{name} ({n}){ext}".to_owned(),
            start: 1,
        }
    }
}

/// One item's new name, and why it can't be used, if so.
#[derive(Facet, Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct BatchItem {
    pub old: String,
    pub new: String,
    /// "" when the name can be used.
    pub problem: String,
}

#[derive(Facet, Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct BatchRenameView {
    pub fields: BatchFields,
    pub items: Vec<BatchItem>,
    /// Items whose name changes.
    pub changes: u32,
    /// Items whose new name can't be used: renaming waits until there are none.
    pub problems: u32,
}

/// An item being renamed: where it is, and whether it is a folder (folders
/// have no extension).
#[derive(Clone, Debug)]
struct Item {
    path: PathBuf,
    folder: bool,
}

/// The dialog's state, while it is open.
#[derive(Clone, Debug)]
pub(super) struct BatchRename {
    items: Vec<Item>,
    fields: BatchFields,
    /// The names already in each item's folder, as the listing has them.
    taken: Vec<PathBuf>,
}

fn name_of(path: &Path) -> String {
    path.file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default()
}

impl BatchRename {
    /// Each item's new name, in order.
    // `{name}` and friends are the pattern's own placeholders.
    #[allow(clippy::literal_string_with_formatting_args)]
    fn names(&self) -> Vec<String> {
        let fields = &self.fields;
        let first = fields.start as usize;
        let width = (first + self.items.len()).to_string().len();
        self.items
            .iter()
            .enumerate()
            .map(|(index, item)| {
                let name = name_of(&item.path);
                match fields.mode {
                    BatchMode::Replace if fields.find.is_empty() => name,
                    BatchMode::Replace => name.replace(&fields.find, &fields.replace),
                    BatchMode::Pattern => {
                        let (stem, ext) = match name.rfind('.') {
                            Some(dot) if dot > 0 && !item.folder => name.split_at(dot),
                            _ => (name.as_str(), ""),
                        };
                        let number = format!("{:0width$}", first + index);
                        fields
                            .pattern
                            .replace("{name}", stem)
                            .replace("{ext}", ext)
                            .replace("{n}", &number)
                    }
                }
            })
            .collect()
    }

    pub(super) fn view(&self) -> BatchRenameView {
        let names = self.names();
        let items: Vec<BatchItem> = self
            .items
            .iter()
            .zip(&names)
            .map(|(item, new)| {
                let target = item.path.with_file_name(new);
                let problem = validate_name(new).err().or_else(|| {
                    if names.iter().filter(|other| *other == new).count() > 1 {
                        Some("Two items would get this name.")
                    } else if target != item.path
                        && self.taken.contains(&target)
                        && !self.items.iter().any(|other| other.path == target)
                    {
                        Some("An item with this name already exists.")
                    } else {
                        None
                    }
                });
                BatchItem {
                    old: name_of(&item.path),
                    new: new.clone(),
                    problem: problem.unwrap_or_default().to_owned(),
                }
            })
            .collect();
        let count = |keep: &dyn Fn(&BatchItem) -> bool| {
            u32::try_from(items.iter().filter(|item| keep(item)).count()).unwrap_or(u32::MAX)
        };
        BatchRenameView {
            fields: self.fields.clone(),
            changes: count(&|item| item.old != item.new),
            problems: count(&|item| !item.problem.is_empty()),
            items,
        }
    }
}

impl Tab {
    /// Batch rename for the selection, when several items are selected.
    pub(super) fn start_batch(&self) -> Option<BatchRename> {
        let rows: Vec<usize> = self.selection.rows().collect();
        if rows.len() < 2 || self.trash {
            return None;
        }
        let items = rows
            .into_iter()
            .filter_map(|row| {
                let entry = *self.order.get(row)? as usize;
                Some(Item {
                    path: self.row_path(row)?,
                    folder: self.listing.meta(entry).entry_type == EntryType::Directory,
                })
            })
            .collect();
        let taken = (0..self.order.len())
            .filter_map(|row| self.row_path(row))
            .collect();
        Some(BatchRename {
            items,
            fields: BatchFields::default(),
            taken,
        })
    }
}

impl DirFleet {
    /// The events of the batch rename dialog.
    pub(super) fn batch_event(model: &mut Model, event: Event) -> Command<Effect, Event> {
        let tab = match &event {
            Event::StartBatchRename { tab }
            | Event::SetBatchRename { tab, .. }
            | Event::ApplyBatchRename { tab }
            | Event::CancelBatchRename { tab } => *tab,
            _ => return Command::done(),
        };
        let Some(id) = tab_id(tab).filter(|id| model.tabs.contains_key(id)) else {
            return Command::done();
        };
        let state = model.tabs.get_mut(&id).expect("checked above");
        match event {
            Event::StartBatchRename { .. } => state.batch = state.start_batch(),
            Event::SetBatchRename { fields, .. } => {
                if let Some(batch) = state.batch.as_mut() {
                    batch.fields = fields;
                }
            }
            Event::CancelBatchRename { .. } => state.batch = None,
            Event::ApplyBatchRename { .. } => {
                let Some(batch) = state.batch.as_ref() else {
                    return Command::done();
                };
                let view = batch.view();
                if view.problems > 0 || view.changes == 0 {
                    return Command::done();
                }
                let pairs: Vec<(PathBuf, PathBuf)> = batch
                    .items
                    .iter()
                    .zip(view.items)
                    .filter(|(_, item)| item.old != item.new)
                    .map(|(item, new)| (item.path.clone(), item.path.with_file_name(new.new)))
                    .collect();
                state.select_after = pairs.iter().map(|(_, to)| to.clone()).collect();
                state.batch = None;
                return Self::run(
                    model,
                    FileOp::RenameMany(pairs),
                    Origin::User,
                    Some(id),
                    false,
                );
            }
            _ => {}
        }
        render()
    }
}
